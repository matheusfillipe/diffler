//! Diff/review screen state and handlers: a file sidebar listing every file
//! in the diff, and a pane showing only the selected file's hunks, lines, and
//! inline comments, flattened into a row list so the renderer only ever
//! materializes the visible slice.

mod comments;
mod nav;
mod open;
mod review;
mod rows;

use std::collections::{BTreeSet, HashMap, HashSet};

use diffler_core::classify::{Kind, Rules};
use diffler_core::highlight::StyledRange;
use diffler_core::model::DiffModel;
use diffler_core::review::Review;
use diffler_core::session::Session;
use diffler_core::source::ReviewSource;
use diffler_core::syntax::ScopeIndex;

use super::composer::{Composer, ComposerKind};
use super::{App, Flow};
pub use rows::{CommentLine, DiffRow, SplitRow, SplitSide, comment_display};
use rows::{build_rows, build_split_rows};

use crate::config::FileLayout;
use crate::tree::{self, Bucket, TreeNode, TreeRow};

/// Which pane has the keyboard: the file sidebar or the diff body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    List,
    Diff,
    /// The comments sidebar on the right, open only while `comments_open`.
    Comments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAlign {
    Center,
    Top,
    Bottom,
}

/// Which section headers are collapsed, across both layouts that have them.
/// What the reader did not come to read starts folded: the viewed pile, and
/// the two kinds nobody reviews by hand.
#[derive(Debug, Clone)]
pub(crate) struct BucketFolds(BTreeSet<Bucket>);

impl Default for BucketFolds {
    fn default() -> Self {
        Self(
            [
                Bucket::Viewed,
                Bucket::Kind(Kind::Generated),
                Bucket::Kind(Kind::Assets),
            ]
            .into(),
        )
    }
}

impl BucketFolds {
    fn is_folded(&self, bucket: Bucket) -> bool {
        self.0.contains(&bucket)
    }

    fn toggle_fold(&mut self, bucket: Bucket) {
        if !self.0.remove(&bucket) {
            self.0.insert(bucket);
        }
    }

    /// Open a section, reporting whether it had been folded.
    fn unfold(&mut self, bucket: Bucket) -> bool {
        self.0.remove(&bucket)
    }
}

/// Per-line syntax spans for both sides of one file, keyed by the
/// both-sides content hash so edits to either side invalidate naturally.
/// Filled by the background enrichment worker.
#[derive(Debug, Clone)]
pub struct FileHighlights {
    pub hash: String,
    pub old: Vec<Vec<StyledRange>>,
    pub new: Vec<Vec<StyledRange>>,
}

/// Enclosing-definition index for one file's new-side content, keyed by the
/// both-sides hash so edits invalidate it. Drives the sticky scope breadcrumb.
#[derive(Debug)]
pub struct FileScope {
    pub hash: String,
    pub index: ScopeIndex,
}

#[allow(clippy::struct_excessive_bools)] // independent view toggles, not a state enum
pub struct DiffView {
    pub source: ReviewSource,
    /// A commit's diff is immutable: fetched once at open and kept here.
    /// `None` means the view reads the live `review.model`.
    pub(crate) commit_model: Option<DiffModel>,
    pub focus: Pane,
    /// File-list layout for the sidebar: a flat list or a collapsible tree.
    /// Pinned at open from `ui.diff_file_layout`.
    pub(crate) layout: FileLayout,
    /// Index into `model.files`: the file shown in the diff pane. Derived from
    /// the file under `tree_cursor` whenever that lands on a File row.
    pub selected: usize,
    /// Folded directory paths in the sidebar tree; persists across refresh.
    /// Unused in the flat list (it has no directories).
    pub(crate) folded_dirs: BTreeSet<String>,
    /// Section-header folds, shared by the review and kinds layouts.
    pub(crate) bucket_folds: BucketFolds,
    /// How the kinds layout buckets a path: the built-in table under the
    /// reader's `[classify]` globs. Pinned at open.
    pub(crate) rules: Rules,
    /// What the repo's own git attributes declare, for the files of the diff
    /// the kinds layout is showing. Filled from the backend when that layout
    /// is on, so the per-frame row build stays a pure path lookup.
    pub(crate) declared: HashMap<String, Kind>,
    /// Cursor into the current visible sidebar tree rows (dirs and files).
    pub(crate) tree_cursor: usize,
    /// Row within the selected file's rows.
    pub cursor: usize,
    /// First visible row of the diff pane; the renderer keeps the cursor in
    /// view.
    pub scroll: usize,
    /// Applied once the renderer knows the wrapped row heights, then cleared.
    pub(crate) scroll_align: Option<ScrollAlign>,
    /// Side-by-side (old left / new right) pane; pinned at open from
    /// `ui.side_by_side`, then `|` toggles it live.
    pub side_by_side: bool,
    /// First visible split row while `side_by_side` is on; the renderer keeps
    /// the cursor's line in view.
    pub(crate) split_scroll: usize,
    /// Last render's sidebar/pane rects and sidebar scroll, for mouse
    /// hit-testing. The pane's own scroll is `scroll` / `split_scroll`.
    pub(crate) sidebar: ratatui::layout::Rect,
    pub(crate) sidebar_scroll: usize,
    pub(crate) pane: ratatui::layout::Rect,
    /// The comments sidebar: open state, which comment is selected, and the
    /// last render's rect and scroll for hit-testing, mirroring the file list.
    pub(crate) comments_open: bool,
    pub(crate) comments_cursor: usize,
    pub(crate) comments_scroll: usize,
    pub(crate) comments_rect: ratatui::layout::Rect,
    /// Last render's comment-sidebar line -> comment index, so a click on any
    /// wrapped body line selects the comment it belongs to.
    pub(crate) comment_lines: Vec<Option<usize>>,
    /// Row where `V` started; `Some` means line selection is active.
    pub visual_anchor: Option<usize>,
    /// Body height of the last diff-pane render, drives half-page motions.
    pub(crate) viewport: u16,
    /// The open in-place comment editor, if any. It owns the diff pane's keys
    /// while it is up and occupies the rows its result will.
    pub(crate) composer: Option<Composer>,
    /// Rows for the selected file only.
    pub(crate) rows: Vec<DiffRow>,
    /// Last render's pane line -> row index table; wrapped rows span several
    /// lines, so mouse hits map back through it.
    pub(crate) line_rows: Vec<Option<usize>>,
    rows_dirty: bool,
    /// Pane row width comments wrap to; set from the last draw, MAX until
    /// the first frame so unit tests see unwrapped lines.
    pub(crate) wrap_width: u16,
    /// Rows need a re-flow for a new wrap width (no content change).
    wrap_dirty: bool,
    /// Side-by-side row model, rebuilt with `rows` (not per frame).
    pub(crate) split_rows: Vec<SplitRow>,
    pub(crate) highlights: HashMap<String, FileHighlights>,
    pub(crate) scopes: HashMap<String, FileScope>,
    /// Paths whose intra-line emphasis has been computed, so the per-file
    /// enrichment runs once. Cleared whenever the underlying model is
    /// rebuilt (refresh) so a fresh unenriched file gets re-enriched.
    enriched: HashSet<String>,
    /// Per-file diff context override (path -> git context lines, `u32::MAX`
    /// for the whole file). Absent means the source's default context.
    pub(crate) context: HashMap<String, u32>,
}

impl DiffView {
    fn new(
        source: ReviewSource,
        commit_model: Option<DiffModel>,
        review: &Review,
        layout: FileLayout,
        rules: Rules,
        side_by_side: bool,
    ) -> Self {
        let mut view = Self {
            composer: None,
            source,
            commit_model,
            focus: Pane::List,
            layout,
            selected: 0,
            folded_dirs: BTreeSet::new(),
            bucket_folds: BucketFolds::default(),
            rules,
            declared: HashMap::new(),
            tree_cursor: 0,
            cursor: 0,
            scroll: 0,
            scroll_align: None,
            side_by_side,
            split_scroll: 0,
            sidebar: ratatui::layout::Rect::default(),
            sidebar_scroll: 0,
            pane: ratatui::layout::Rect::default(),
            comments_open: false,
            comments_cursor: 0,
            comments_scroll: 0,
            comments_rect: ratatui::layout::Rect::default(),
            comment_lines: Vec::new(),
            visual_anchor: None,
            viewport: 0,
            rows: Vec::new(),
            line_rows: Vec::new(),
            rows_dirty: true,
            wrap_width: u16::MAX,
            wrap_dirty: false,
            split_rows: Vec::new(),
            highlights: HashMap::new(),
            scopes: HashMap::new(),
            enriched: HashSet::new(),
            context: HashMap::new(),
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
    pub(crate) fn is_enriched(&self, path: &str) -> bool {
        self.enriched.contains(path)
    }

    pub(crate) fn mark_enriched(&mut self, path: &str) {
        self.enriched.insert(path.to_owned());
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
            DiffRow::Composer { line } => (
                split
                    .iter()
                    .position(|r| matches!(r, SplitRow::Composer { line: l } if *l == line))
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

    /// Mark the row list stale, dropping any visual anchor (a row index that
    /// would dangle across a rebuild). Enrichment caches survive.
    pub(crate) fn mark_rows_dirty(&mut self) {
        self.rows_dirty = true;
        self.visual_anchor = None;
    }

    /// Mark rows stale and forget enrichment, so a rebuilt model re-enriches.
    pub(crate) fn invalidate(&mut self) {
        self.mark_rows_dirty();
        self.enriched.clear();
    }

    /// Adopt the diff pane's row width so comment text wraps to it; rows
    /// re-flow when it changes (a resize, the sidebar layout toggling).
    pub(crate) fn set_wrap_width(&mut self, width: u16) {
        if width > 0 && self.wrap_width != width {
            self.wrap_width = width;
            self.mark_reflow();
        }
    }

    /// Rebuild the pane's rows, leaving the sidebar's file list as it is.
    pub(crate) fn mark_reflow(&mut self) {
        self.wrap_dirty = true;
    }

    pub(crate) fn ensure_rows(&mut self, review: &Review) {
        if !self.rows_dirty && !self.wrap_dirty {
            return;
        }
        let model = self.commit_model.as_ref().unwrap_or_else(|| review.model());
        self.selected = self.selected.min(model.files.len().saturating_sub(1));
        let session = review.session_for(&self.source);
        let composer = self.composer.as_ref();
        self.rows = build_rows(model, session, self.selected, self.wrap_width, composer);
        self.split_rows =
            build_split_rows(model, session, self.selected, self.wrap_width, composer);
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
        // the file list may have shifted (refresh) or folds may hide the old
        // cursor row: keep the tree cursor on the pane's file. A pure wrap
        // re-flow changes neither, and must not move a browsing cursor.
        if self.rows_dirty {
            let tree_rows = self.tree_rows(model, session);
            self.reseat_tree_cursor(&tree_rows);
        }
        self.rows_dirty = false;
        self.wrap_dirty = false;
        self.seat_cursor_on_caret();
    }

    /// Park the pane cursor on the composer's caret row, so the pane scrolls
    /// to the text being written and holds it there when a rebuild shifts the
    /// card's rows.
    fn seat_cursor_on_caret(&mut self) {
        let Some(composer) = self.composer.as_ref() else {
            return;
        };
        let caret = composer.caret_line(self.wrap_width);
        if let Some(row) = self
            .rows
            .iter()
            .position(|row| matches!(row, DiffRow::Composer { line } if *line == caret))
        {
            self.cursor = row;
        }
    }

    /// Open whatever hides the selected file in the sidebar: its ancestor
    /// directories in the tree layout, its section in the ones with headers.
    /// Only the layout on screen is touched, so the others keep their folds.
    pub(crate) fn reveal_selected(&mut self, review: &Review) {
        let model = self.model(review);
        let Some(file) = model.files.get(self.selected) else {
            return;
        };
        let path = file.path.clone();
        let viewed = review
            .session_for(&self.source)
            .is_viewed(&path, &file.content_hash());
        let revealed = match self.layout {
            FileLayout::Review => {
                let bucket = if viewed {
                    Bucket::Viewed
                } else {
                    Bucket::ToReview
                };
                self.bucket_folds.unfold(bucket)
            }
            FileLayout::Kinds => {
                let bucket = Bucket::Kind(self.kind_of(&path));
                self.bucket_folds.unfold(bucket)
            }
            FileLayout::Tree | FileLayout::List => {
                let hidden_by = |dir: &String| path.starts_with(&format!("{dir}/"));
                let hidden = self.folded_dirs.iter().any(hidden_by);
                if hidden {
                    self.folded_dirs.retain(|dir| !hidden_by(dir));
                }
                hidden
            }
        };
        self.rows_dirty |= revealed;
    }

    /// Seat the tree cursor on the selected file's row, or clamp it into
    /// range when that row is hidden (folded away, moved between buckets).
    pub(crate) fn reseat_tree_cursor(&mut self, rows: &[TreeRow]) {
        self.tree_cursor = tree_position_of_file(rows, self.selected)
            .unwrap_or_else(|| self.tree_cursor.min(rows.len().saturating_sub(1)));
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
    /// set). The review layout splits files into a to-review and a viewed
    /// bucket under foldable Section rows; bucket membership reads the
    /// hash-keyed viewed marks, so an edited file falls back into to-review by
    /// itself. The kinds layout groups them by what they are. Files keep their
    /// model index.
    ///
    /// Inside a group, whether a directory or a section, the files already
    /// viewed come first: every layout sorts the same way the review layout's
    /// buckets do, so what is left to read is one run at the bottom of each
    /// group and marking a file moves it out of that run.
    pub(crate) fn tree_rows(&self, model: &DiffModel, session: &Session) -> Vec<TreeRow> {
        match self.layout {
            FileLayout::Review => self.section_rows(model, Self::review_groups(model, session)),
            FileLayout::Kinds => self.section_rows(model, self.kind_groups(model, session)),
            // list belongs to the status screen (config rejects it here); a
            // stray value degrades to the tree
            FileLayout::Tree | FileLayout::List => {
                tree::visible_rows_promoting(&Self::paths(model), &self.folded_dirs, &|index| {
                    Self::is_viewed(model, session, index)
                })
            }
        }
    }

    /// Every file the sidebar lists, in the order it lists them, as if nothing
    /// were folded. A reader walks the order on screen, so the commands that
    /// step files read it from here.
    pub(crate) fn display_order(&self, model: &DiffModel, session: &Session) -> Vec<usize> {
        match self.layout {
            FileLayout::Review => Self::review_groups(model, session),
            FileLayout::Kinds => self.kind_groups(model, session),
            FileLayout::Tree | FileLayout::List => {
                let rows =
                    tree::visible_rows_promoting(&Self::paths(model), &BTreeSet::new(), &|index| {
                        Self::is_viewed(model, session, index)
                    });
                return rows.iter().filter_map(row_file_index).collect();
            }
        }
        .into_iter()
        .flat_map(|(_, indices)| indices)
        .collect()
    }

    fn paths(model: &DiffModel) -> Vec<&str> {
        model.files.iter().map(|file| file.path.as_str()).collect()
    }

    fn is_viewed(model: &DiffModel, session: &Session, index: usize) -> bool {
        model
            .files
            .get(index)
            .is_some_and(|file| session.is_viewed(&file.path, &file.content_hash()))
    }

    /// Order a group's files for display: viewed first, each run keeping the
    /// order the diff gave it.
    fn viewed_first(model: &DiffModel, session: &Session, indices: &mut [usize]) {
        indices.sort_by_key(|&index| !Self::is_viewed(model, session, index));
    }

    /// A header row per group, its files beneath it unless it is folded. The
    /// renderer reads the depths to draw the indent, so both grouped layouts
    /// emit them from here.
    fn section_rows(
        &self,
        model: &DiffModel,
        groups: impl IntoIterator<Item = (Bucket, Vec<usize>)>,
    ) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for (bucket, indices) in groups {
            let folded = self.bucket_folds.is_folded(bucket);
            rows.push(TreeRow {
                depth: 0,
                node: TreeNode::Section {
                    bucket,
                    count: indices.len(),
                    folded,
                },
            });
            if folded {
                continue;
            }
            rows.extend(indices.into_iter().filter_map(|index| {
                let file = model.files.get(index)?;
                Some(TreeRow {
                    depth: 1,
                    node: TreeNode::File {
                        index,
                        name: file.path.clone(),
                    },
                })
            }));
        }
        rows
    }

    fn review_groups(model: &DiffModel, session: &Session) -> Vec<(Bucket, Vec<usize>)> {
        let (mut fresh, mut viewed) = (Vec::new(), Vec::new());
        for index in 0..model.files.len() {
            if Self::is_viewed(model, session, index) {
                viewed.push(index);
            } else {
                fresh.push(index);
            }
        }
        vec![(Bucket::ToReview, fresh), (Bucket::Viewed, viewed)]
    }

    /// One group per kind that the diff has files for, in [`Kind::ALL`] order.
    /// Files keep their model index, and their order within the diff behind
    /// the ones already viewed.
    fn kind_groups(&self, model: &DiffModel, session: &Session) -> Vec<(Bucket, Vec<usize>)> {
        // `Kind::ALL` below is the display order
        let mut grouped: HashMap<Kind, Vec<usize>> = HashMap::new();
        for (index, file) in model.files.iter().enumerate() {
            grouped
                .entry(self.kind_of(&file.path))
                .or_default()
                .push(index);
        }
        Kind::ALL
            .into_iter()
            .filter_map(|kind| {
                let mut indices = grouped.remove(&kind)?;
                Self::viewed_first(model, session, &mut indices);
                Some((Bucket::Kind(kind), indices))
            })
            .collect()
    }

    pub(crate) fn kind_of(&self, path: &str) -> Kind {
        self.rules.kind(path, self.declared.get(path).copied())
    }

    /// The paths a declared-kinds worker should ask git about, empty unless
    /// the kinds layout is the one on screen.
    fn declared_targets(&self, review: &Review) -> Vec<String> {
        if self.layout != FileLayout::Kinds {
            return Vec::new();
        }
        self.model(review)
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect()
    }

    /// Advance the sidebar layout: tree → review → kinds → tree.
    pub(crate) fn cycle_layout(&mut self) -> FileLayout {
        self.layout = match self.layout {
            FileLayout::Review => FileLayout::Kinds,
            FileLayout::Kinds => FileLayout::Tree,
            _ => FileLayout::Review,
        };
        self.layout
    }
}

/// The sidebar rows for `diff`, pairing its model with its source's session
/// (the review layout needs the viewed marks).
fn sidebar_rows(diff: &DiffView, review: &Review) -> Vec<TreeRow> {
    diff.tree_rows(diff.model(review), review.session_for(&diff.source))
}

/// Model index of the next file after the selection not yet marked viewed,
/// scanning forward and coming back around only when `wrap` asks for it.
fn next_unviewed_index(diff: &DiffView, review: &Review, wrap: bool) -> Option<usize> {
    let model = diff.model(review);
    let session = review.session_for(&diff.source);
    // the order on screen: a jump has to land where the reader can see it came
    // from
    let order = diff.display_order(model, session);
    let count = order.len();
    let at = order.iter().position(|&index| index == diff.selected)?;
    (1..=count)
        .map(|step| at + step)
        .take_while(|&position| wrap || position < count)
        .filter_map(|position| order.get(position % count).copied())
        .find(|&index| !DiffView::is_viewed(model, session, index))
}

/// The model file a row addresses, for the walks that step files past the
/// directory and section headers between them.
fn row_file_index(row: &TreeRow) -> Option<usize> {
    match row.node {
        TreeNode::File { index, .. } => Some(index),
        TreeNode::Dir { .. } | TreeNode::Section { .. } => None,
    }
}

/// Visible-row index of the File row addressing `file_index`, if shown.
fn tree_position_of_file(rows: &[TreeRow], file_index: usize) -> Option<usize> {
    rows.iter()
        .position(|row| row_file_index(row) == Some(file_index))
}

/// Row position of the File row next to `at`, `forward` down the list or back
/// up it, skipping the headers in between.
fn step_file_row(rows: &[TreeRow], at: usize, forward: bool) -> Option<usize> {
    let rows = rows.iter().enumerate();
    if forward {
        rows.skip(at + 1)
            .find(|(_, row)| row_file_index(row).is_some())
    } else {
        rows.take(at)
            .rfind(|(_, row)| row_file_index(row).is_some())
    }
    .map(|(position, _)| position)
}

/// Paths whose git attributes a worker should read, for the kinds sidebar.
#[derive(Debug, Clone)]
pub struct DeclaredRequest {
    pub paths: Vec<String>,
    /// The request this answers; an answer for a file list the view has since
    /// replaced is dropped.
    pub token: u64,
}

impl App {
    /// Ask git what the repo declares about the kinds sidebar's files. The
    /// lookup walks the attribute files per path, so it runs on a worker and
    /// the sidebar groups by the built-in table until the answer lands.
    pub(crate) fn queue_declared(&mut self) {
        let paths = self
            .diff
            .as_ref()
            .map(|diff| diff.declared_targets(&self.review))
            .unwrap_or_default();
        self.declared_token += 1;
        let token = self.declared_token;
        self.pending_declared = (!paths.is_empty()).then_some(DeclaredRequest { paths, token });
    }

    pub(crate) fn on_declared_kinds(&mut self, kinds: HashMap<String, Kind>, token: u64) -> Flow {
        let Some(diff) = self.diff.as_mut().filter(|_| token == self.declared_token) else {
            return Flow::Idle;
        };
        if diff.declared == kinds {
            return Flow::Idle;
        }
        diff.declared = kinds;
        diff.invalidate();
        diff.ensure_rows(&self.review);
        Flow::Continue
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::feedback::{self, FeedbackOptions};
    use diffler_core::model::LineKind;
    use diffler_core::session::{Anchor, CommentStatus};
    use unicode_width::UnicodeWidthStr;

    use crate::app::markdown::MdSpan;

    use super::*;
    use crate::app::{App, Modal, Screen};
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{
        Fixture, code_key, ctrl_key, key, standard_fixture, two_hunk_fixture,
    };

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
        sidebar_rows(diff, &app.review).len()
    }

    /// Kinds of the visible sidebar tree rows: "dir", "file:<name>", or
    /// "section:<label>:<count>".
    fn tree_kinds(app: &App) -> Vec<String> {
        let diff = app.diff.as_ref().expect("diff view");
        sidebar_rows(diff, &app.review)
            .iter()
            .map(|row| match &row.node {
                crate::tree::TreeNode::Dir { .. } => "dir".to_owned(),
                crate::tree::TreeNode::File { name, .. } => format!("file:{name}"),
                crate::tree::TreeNode::Section { bucket, count, .. } => {
                    format!("section:{}:{count}", bucket.label())
                }
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

    /// A review of nothing is a screen with no rows to read or comment on, so
    /// the opener declines and the reader stays where the answer is.
    #[test]
    fn opening_a_review_with_no_files_says_so_and_opens_nothing() {
        let fixture = Fixture::new();
        fixture.write("a.rs", "fn a() {}\n");
        fixture.commit_all("base");
        let mut app = App::new(fixture.review(), LoadedConfig::default());

        app.handle(key('D'));
        assert!(app.diff.is_none(), "no review opened over a clean tree");
        assert_eq!(app.screen(), Screen::Status);
        assert_eq!(
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("nothing to review: working tree clean")
        );

        // an empty commit reaches the same guard, named by its own source
        fixture.commit_all("nothing");
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        app.open_commit_diff(&head);
        assert!(app.diff.is_none(), "no review opened over an empty commit");
        let message = app.message.as_ref().expect("message").text.clone();
        assert!(
            message.starts_with("nothing to review in commit"),
            "the source names itself: {message}"
        );
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
    fn gg_and_g_jump_to_the_first_and_last_visible_row() {
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
    fn h_and_l_focus_the_panes_and_clamp() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        assert_eq!(focus(&app), Pane::List);
        app.handle(key('l'));
        assert_eq!(focus(&app), Pane::Diff);
        app.handle(key('l'));
        assert_eq!(
            focus(&app),
            Pane::Diff,
            "repeats stay on the diff, no cycle"
        );
        app.handle(key('h'));
        assert_eq!(focus(&app), Pane::List);
        app.handle(key('h'));
        assert_eq!(
            focus(&app),
            Pane::List,
            "repeats stay on the sidebar, no cycle"
        );
    }

    #[test]
    fn arrow_keys_focus_the_panes_like_h_and_l() {
        use crossterm::event::KeyCode;
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.handle(code_key(KeyCode::Right));
        assert_eq!(focus(&app), Pane::Diff);
        app.handle(code_key(KeyCode::Left));
        assert_eq!(focus(&app), Pane::List);
    }

    #[test]
    fn tab_on_a_file_at_the_repo_root_says_there_is_nothing_to_fold() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        // the tree opens on ci.yml, which sits in no folder
        let before = tree_kinds(&app);

        app.handle(key('\t'));

        assert_eq!(tree_kinds(&app), before, "nothing folded");
        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.contains("nothing to fold")),
            "{:?}",
            app.message
        );
    }

    #[test]
    fn tab_folds_the_directory_the_sidebar_cursor_sits_in() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        // onto src/lib.rs, a file inside the only folder
        app.handle(key('k'));
        let file = selected_path(&app);
        assert!(tree_kinds(&app).contains(&"file:lib.rs".to_owned()));

        app.handle(key('\t'));

        assert!(
            !tree_kinds(&app).contains(&"file:lib.rs".to_owned()),
            "tab folds the folder away: {:?}",
            tree_kinds(&app)
        );
        assert_eq!(selected_path(&app), file, "and steps to no other file");
        // the cursor rode up to the folder row, so the same key opens it again
        app.handle(key('\t'));
        assert!(tree_kinds(&app).contains(&"file:lib.rs".to_owned()));
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
        // tree: dir src, lib.rs, ci.yml, todo.md. c-n/c-p never land on a dir
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
    fn brace_keys_walk_between_comments() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let anchor = |line: u32| Anchor {
            file: "data.txt".to_owned(),
            line: Some(line),
            line_end: None,
            on_old_side: false,
            line_text: None,
        };
        app.review.session.add_comment(anchor(1), "r", "first");
        app.review.session.add_comment(anchor(20), "r", "second");
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
        app.handle(key('}'));
        assert!(on_header(&app), "}} lands on a comment header");
        let first = app.diff.as_ref().unwrap().cursor;
        app.handle(key('}'));
        assert!(on_header(&app));
        let second = app.diff.as_ref().unwrap().cursor;
        assert!(second > first, "}} advances to the next comment");
        app.handle(key('{'));
        assert_eq!(
            app.diff.as_ref().unwrap().cursor,
            first,
            "{{ returns to the previous comment"
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
        assert!(app.composer_open());
        type_text(&mut app, "why 42?");
        app.handle(key('\n'));
        assert!(!app.composer_open());

        let comment = &app.review.session.comments[0];
        assert_eq!(comment.author, "reviewer");
        assert_eq!(comment.body, "why 42?");
        assert_eq!(comment.anchor.file, "src/lib.rs");
        assert_eq!(comment.anchor.line, Some(2));
        assert!(!comment.anchor.on_old_side);
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
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: Some("    43".to_owned()),
            },
            "reviewer",
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
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(99),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
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
        assert!(app.composer_open());
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

    /// Row indices of the composer's rows, in order.
    fn composer_rows(app: &App) -> Vec<usize> {
        app.diff
            .as_ref()
            .expect("diff view")
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, DiffRow::Composer { .. }))
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn the_composer_takes_the_rows_under_the_line_it_comments_on() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let line = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = line;
        app.handle(key('c'));
        let rows = composer_rows(&app);
        assert_eq!(
            rows.first().copied(),
            Some(line + 1),
            "the card opens directly under its line"
        );
        assert!(rows.len() >= 3, "header, a body row, and the hint footer");
        assert!(
            rows.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "the card is one contiguous block: {rows:?}"
        );
    }

    #[test]
    fn the_composer_grows_a_row_when_the_text_wraps() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.diff.as_mut().unwrap().set_wrap_width(60);
        app.handle(key('c'));
        let before = composer_rows(&app).len();
        type_text(
            &mut app,
            &"x".repeat(super::super::composer::card_budget(60) + 1),
        );
        assert_eq!(
            composer_rows(&app).len(),
            before + 1,
            "one more row once the text passes the wrap budget"
        );
    }

    /// The composer wraps the raw buffer per character and the finished card
    /// word-wraps its markdown, so they only have to agree on the budget. An
    /// unbroken token exactly that wide is where a mismatch shows.
    #[test]
    fn a_draft_and_the_comment_it_becomes_wrap_to_the_same_budget() {
        let width = 48;
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.diff.as_mut().unwrap().set_wrap_width(width);
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('c'));
        type_text(
            &mut app,
            &"w".repeat(super::super::composer::card_budget(width)),
        );
        let drafted = composer_rows(&app).len();
        app.handle(key('\n'));
        app.diff.as_mut().unwrap().ensure_rows(&app.review);
        let landed = app
            .diff
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .filter(|row| matches!(row, DiffRow::Comment { .. }))
            .count();
        assert_eq!(drafted, 3, "header, the one full row, footer");
        assert_eq!(
            drafted, landed,
            "writing a comment and reading it back take the same rows"
        );
    }

    /// Put the sidebar cursor on the tree row for `dir`.
    fn cursor_to_dir(app: &mut App, dir: &str) {
        let review = &app.review;
        let diff = app.diff.as_ref().expect("diff view");
        let rows = super::sidebar_rows(diff, review);
        let at = rows
            .iter()
            .position(
                |row| matches!(&row.node, crate::tree::TreeNode::Dir { path, .. } if path == dir),
            )
            .unwrap_or_else(|| panic!("a tree row for {dir}"));
        let diff = app.diff.as_mut().expect("diff view");
        diff.focus = Pane::List;
        diff.tree_cursor = at;
    }

    fn viewed_paths(app: &App) -> Vec<String> {
        let source = app.active_review_source();
        let session = app.review.session_for(&source);
        let mut out: Vec<String> = app
            .diff
            .as_ref()
            .unwrap()
            .model(&app.review)
            .files
            .iter()
            .filter(|f| session.is_viewed(&f.path, &f.content_hash()))
            .map(|f| f.path.clone())
            .collect();
        out.sort();
        out
    }

    /// Two files directly under `src/`, one nested a level deeper, and one
    /// outside it entirely, so a subtree mark has something real to cover and
    /// something to leave alone.
    fn nested_fixture() -> Fixture {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub fn a() {}\n");
        fixture.write("src/main.rs", "fn main() {}\n");
        fixture.write("src/deep/inner.rs", "pub fn b() {}\n");
        fixture.write("docs/readme.md", "hi\n");
        fixture.commit_all("initial");
        fixture.write("src/lib.rs", "pub fn a() -> u32 {\n    1\n}\n");
        fixture.write("src/main.rs", "fn main() {\n    println!();\n}\n");
        fixture.write("src/deep/inner.rs", "pub fn b() -> u32 {\n    2\n}\n");
        fixture.write("docs/readme.md", "hi there\n");
        fixture
    }

    #[test]
    fn viewing_a_directory_marks_the_whole_subtree_and_nothing_outside_it() {
        let fixture = nested_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
        cursor_to_dir(&mut app, "src");
        app.handle(key('v'));
        assert_eq!(
            viewed_paths(&app),
            ["src/deep/inner.rs", "src/lib.rs", "src/main.rs"],
            "every file under src, including the nested one"
        );
    }

    #[test]
    fn viewing_a_nested_directory_leaves_its_parent_alone() {
        let fixture = nested_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
        cursor_to_dir(&mut app, "src/deep");
        app.handle(key('v'));
        assert_eq!(viewed_paths(&app), ["src/deep/inner.rs"]);
    }

    #[test]
    fn viewing_an_already_viewed_directory_puts_it_back() {
        let fixture = standard_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
        cursor_to_dir(&mut app, "src");
        app.handle(key('v'));
        assert!(!viewed_paths(&app).is_empty());
        cursor_to_dir(&mut app, "src");
        app.handle(key('v'));
        assert!(
            viewed_paths(&app).is_empty(),
            "a second press unmarks the subtree"
        );
    }

    #[test]
    fn viewing_a_file_row_still_marks_only_that_file() {
        let fixture = standard_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
        select_file(&mut app, "src/lib.rs");
        app.handle(key('v'));
        assert_eq!(viewed_paths(&app), ["src/lib.rs"]);
    }

    #[test]
    fn a_file_level_composer_opens_above_the_diff() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        assert_eq!(focus(&app), Pane::List);
        app.handle(key('c'));
        assert_eq!(
            composer_rows(&app).first().copied(),
            Some(0),
            "a whole-file comment is written where it renders, at the top"
        );
    }

    #[test]
    fn a_reply_composer_opens_under_the_thread_it_answers() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let line = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = line;
        app.handle(key('c'));
        type_text(&mut app, "question");
        app.handle(key('\n'));
        app.diff.as_mut().unwrap().cursor = added_line_position(&app) + 1;
        app.handle(key('r'));
        let last_comment = app
            .diff
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .rposition(|row| matches!(row, DiffRow::Comment { .. }))
            .expect("the thread being replied to");
        assert_eq!(
            composer_rows(&app).first().copied(),
            Some(last_comment + 1),
            "the draft reply sits under the whole block"
        );
        assert!(last_comment > line, "and the block sits under its line");
    }

    #[test]
    fn an_edit_composer_stands_in_for_the_block_it_rewrites() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let line = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = line;
        app.handle(key('c'));
        type_text(&mut app, "note");
        app.handle(key('\n'));
        app.diff.as_mut().unwrap().cursor = added_line_position(&app) + 1;
        app.handle(key('c'));
        assert_eq!(composer_rows(&app).first().copied(), Some(line + 1));
        assert!(
            !app.diff
                .as_ref()
                .unwrap()
                .rows()
                .iter()
                .any(|row| matches!(row, DiffRow::Comment { .. })),
            "the comment being edited gives up its rows to the draft"
        );
    }

    #[test]
    fn the_pane_cursor_rides_the_caret_so_a_tall_card_scrolls_with_it() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.diff.as_mut().unwrap().set_wrap_width(60);
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('c'));
        let caret_row = |app: &App| {
            let diff = app.diff.as_ref().unwrap();
            let caret = diff.composer.as_ref().unwrap().caret_line(diff.wrap_width);
            diff.rows()
                .iter()
                .position(|row| matches!(row, DiffRow::Composer { line } if *line == caret))
        };
        assert_eq!(Some(app.diff.as_ref().unwrap().cursor), caret_row(&app));
        // a card taller than any viewport must keep the cursor on the row the
        // text is landing on, not on the header far above it
        for _ in 0..6 {
            type_text(
                &mut app,
                &"y".repeat(super::super::composer::card_budget(60)),
            );
        }
        let rows = composer_rows(&app);
        assert!(rows.len() > 6, "the card grew tall: {}", rows.len());
        assert_eq!(Some(app.diff.as_ref().unwrap().cursor), caret_row(&app));
        assert!(
            app.diff.as_ref().unwrap().cursor > rows[0],
            "the cursor left the header behind"
        );
    }

    #[test]
    fn reopening_the_same_source_keeps_the_draft_on_screen() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        let file = app.diff.as_ref().unwrap().selected;
        assert_ne!(file, 0, "the draft is not on the file a reload defaults to");
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('c'));
        type_text(&mut app, "still writing");
        // a queued open landing on the source already in view
        app.open_working_tree_diff(None);
        assert_eq!(
            app.diff
                .as_ref()
                .and_then(|d| d.composer.as_ref())
                .map(|c| c.buffer.clone()),
            Some("still writing".to_owned())
        );
        assert_eq!(app.diff.as_ref().unwrap().selected, file, "same file");
        assert!(
            !composer_rows(&app).is_empty(),
            "an open composer the pane cannot draw would still eat every key"
        );
    }

    #[test]
    fn esc_closes_the_composer_and_gives_its_rows_back() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('c'));
        type_text(&mut app, "never mind");
        app.handle(crate::test_support::esc_key());
        assert!(!app.composer_open());
        assert!(composer_rows(&app).is_empty());
        assert!(app.review.session.comments.is_empty());
    }

    #[test]
    fn a_blank_composer_submits_as_a_cancel() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('c'));
        type_text(&mut app, "   ");
        app.handle(key('\n'));
        assert!(!app.composer_open());
        assert!(app.review.session.comments.is_empty());
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
        let composer = app
            .diff
            .as_ref()
            .and_then(|d| d.composer.as_ref())
            .expect("the composer opens over the comment");
        assert_eq!(
            composer.buffer, "old note",
            "prefilled with the existing body"
        );
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
            app.composer_open(),
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

    /// The walk follows the sidebar order: the row under the one just marked
    /// is where the reader was heading.
    #[test]
    fn marking_viewed_steps_to_the_file_listed_under_it() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        assert_eq!(
            tree_kinds(&app),
            vec!["dir", "file:lib.rs", "file:ci.yml", "file:todo.md"],
            "directories first, so the sidebar order is not the model order"
        );
        assert_eq!(selected_path(&app), "ci.yml");

        app.handle(key('v'));
        assert!(app.is_path_viewed("ci.yml"));
        assert_eq!(
            selected_path(&app),
            "todo.md",
            "the row under ci.yml, not the next file in the diff"
        );

        app.handle(key('v'));
        assert!(app.is_path_viewed("todo.md"));
        assert_eq!(
            selected_path(&app),
            "todo.md",
            "nothing below: the selection stays"
        );
    }

    /// Three files in one directory, so a group has an order to sort.
    fn grouped_fixture() -> Fixture {
        let fixture = Fixture::new();
        for name in ["a.rs", "b.rs", "c.rs"] {
            fixture.write(&format!("src/{name}"), "fn one() {}\n");
        }
        fixture.commit_all("base");
        for name in ["a.rs", "b.rs", "c.rs"] {
            fixture.write(&format!("src/{name}"), "fn one() -> u8 {\n    1\n}\n");
        }
        fixture
    }

    /// What is done piles at the top of a directory, so the run still to read
    /// is the bottom of the group and never moves out from under the cursor.
    #[test]
    fn a_viewed_file_sorts_to_the_top_of_its_directory() {
        let fixture = grouped_fixture();
        let mut app = diff_app(&fixture);
        assert_eq!(
            tree_kinds(&app),
            vec!["dir", "file:a.rs", "file:b.rs", "file:c.rs"]
        );

        select_file(&mut app, "src/b.rs");
        app.handle(key('v'));
        assert_eq!(
            tree_kinds(&app),
            vec!["dir", "file:b.rs", "file:a.rs", "file:c.rs"],
            "b.rs sorts above what is left to read"
        );
        assert_eq!(
            selected_path(&app),
            "src/c.rs",
            "the row that was under b.rs"
        );

        app.handle(key('v'));
        assert_eq!(
            tree_kinds(&app),
            vec!["dir", "file:b.rs", "file:c.rs", "file:a.rs"],
            "the viewed run keeps the order the diff gave it"
        );
    }

    /// Folding a group is the reader saying "not this one", so the walk stops
    /// at what is on screen and reports what it left behind. `u` is the key
    /// that goes hunting.
    #[test]
    fn the_walk_stops_at_a_folded_group_and_says_what_is_left() {
        let fixture = Fixture::new();
        fixture.write("ci.yml", "on: push\n");
        fixture.write("Cargo.lock", "# generated\n");
        fixture.commit_all("base");
        fixture.write("ci.yml", "on: [push]\n");
        fixture.write("Cargo.lock", "# generated, again\n");
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
        // Generated starts folded, so Cargo.lock has no row of its own
        assert_eq!(
            tree_kinds(&app),
            vec!["section:Config:1", "file:ci.yml", "section:Generated:1"]
        );

        select_file(&mut app, "ci.yml");
        app.handle(key('v'));
        assert_eq!(
            selected_path(&app),
            "ci.yml",
            "the folded section is not walked into"
        );
        assert_eq!(
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("end of the list, 1 still unviewed")
        );

        // u reaches it, folded or not
        app.handle(key('u'));
        assert_eq!(selected_path(&app), "Cargo.lock");
    }

    #[test]
    fn a_viewed_file_sorts_to_the_top_of_its_kind_section() {
        let fixture = grouped_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
        assert_eq!(
            tree_kinds(&app),
            vec![
                "section:Source:3",
                "file:src/a.rs",
                "file:src/b.rs",
                "file:src/c.rs"
            ]
        );

        select_file(&mut app, "src/b.rs");
        app.handle(key('v'));
        assert_eq!(
            tree_kinds(&app),
            vec![
                "section:Source:3",
                "file:src/b.rs",
                "file:src/a.rs",
                "file:src/c.rs"
            ]
        );
        assert_eq!(selected_path(&app), "src/c.rs");
    }

    #[test]
    fn capital_u_clears_all_viewed_marks() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let paths: Vec<String> = app
            .review
            .model()
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect();
        for path in &paths {
            select_file(&mut app, path);
            app.handle(key('v'));
        }
        assert!(
            paths.iter().all(|p| app.is_path_viewed(p)),
            "all marked first"
        );

        app.handle(key('U'));
        assert!(
            paths.iter().all(|p| !app.is_path_viewed(p)),
            "U cleared every viewed mark"
        );
    }

    #[test]
    fn unmarking_viewed_does_not_move_the_selection() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let first = selected_path(&app);
        assert_eq!(first, "ci.yml");
        app.handle(key('v'));
        select_file(&mut app, &first);
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
    fn review_layout_buckets_files_with_the_viewed_pile_folded() {
        let fixture = standard_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
        assert_eq!(
            tree_kinds(&app),
            [
                "section:To review:3",
                "file:ci.yml",
                "file:src/lib.rs",
                "file:todo.md",
                "section:Viewed:0",
            ]
        );
        // v moves the file into the folded viewed bucket and advances
        app.handle(key('v'));
        assert_eq!(
            tree_kinds(&app),
            [
                "section:To review:2",
                "file:src/lib.rs",
                "file:todo.md",
                "section:Viewed:1",
            ]
        );
        assert_eq!(selected_path(&app), "src/lib.rs");
    }

    #[test]
    fn toggling_the_viewed_bucket_reveals_and_hides_its_files() {
        let fixture = standard_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
        app.handle(key('v'));
        // the folded viewed header is the last row; za on it unfolds
        app.handle(key('G'));
        app.handle(key('z'));
        app.handle(key('a'));
        assert_eq!(
            tree_kinds(&app),
            [
                "section:To review:2",
                "file:src/lib.rs",
                "file:todo.md",
                "section:Viewed:1",
                "file:ci.yml",
            ]
        );
        // <cr> on the header folds it back
        app.handle(key('\n'));
        assert_eq!(
            tree_kinds(&app).last().map(String::as_str),
            Some("section:Viewed:1")
        );
    }

    #[test]
    fn editing_a_viewed_file_returns_it_to_the_to_review_bucket() {
        let fixture = standard_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
        app.handle(key('v'));
        assert_eq!(tree_kinds(&app)[0], "section:To review:2");
        // the file changes on disk: its hash no longer matches the mark
        fixture.write("ci.yml", "on: push\njobs: {}\n");
        app.handle(AppEvent::RepoChanged);
        app.settle_refresh();
        assert_eq!(
            tree_kinds(&app)[0],
            "section:To review:3",
            "stale viewed mark drops the file back into to-review"
        );
    }

    #[test]
    fn t_cycles_the_sidebar_through_the_tree_the_review_buckets_and_the_kinds() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        assert!(tree_kinds(&app).contains(&"dir".to_owned()));
        app.handle(key('t'));
        assert_eq!(tree_kinds(&app)[0], "section:To review:3");
        app.handle(key('t'));
        assert_eq!(tree_kinds(&app)[0], "section:Source:1");
        app.handle(key('t'));
        assert!(
            tree_kinds(&app).contains(&"dir".to_owned()),
            "wraps back to the tree"
        );
    }

    #[test]
    fn the_kinds_layout_groups_by_what_a_file_is_and_hides_the_empty_buckets() {
        let fixture = standard_fixture();
        let app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
        assert_eq!(
            tree_kinds(&app),
            vec![
                "section:Source:1",
                "file:src/lib.rs",
                "section:Docs:1",
                "file:todo.md",
                "section:Config:1",
                "file:ci.yml",
            ],
            "one header per kind the diff has, none for the kinds it does not"
        );
    }

    #[test]
    fn the_repos_declared_kinds_land_from_the_worker_and_regroup_the_sidebar() {
        let fixture = standard_fixture();
        let app = &mut diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
        assert!(tree_kinds(app).contains(&"section:Source:1".to_owned()));
        let request = app.pending_declared.take().expect("a queued lookup");
        assert!(request.paths.contains(&"src/lib.rs".to_owned()));

        let kinds = HashMap::from([("src/lib.rs".to_owned(), Kind::Generated)]);
        app.handle(AppEvent::DeclaredKinds {
            kinds,
            token: request.token,
        });

        assert!(
            tree_kinds(app).contains(&"section:Generated:1".to_owned()),
            "{:?}",
            tree_kinds(app)
        );
    }

    #[test]
    fn an_answer_for_a_replaced_file_list_is_dropped() {
        let fixture = standard_fixture();
        let app = &mut diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
        let stale = app.pending_declared.take().expect("a queued lookup").token;
        // the view moves on, so the answer in flight is about the old list
        app.queue_declared();

        app.handle(AppEvent::DeclaredKinds {
            kinds: HashMap::from([("src/lib.rs".to_owned(), Kind::Generated)]),
            token: stale,
        });

        assert!(tree_kinds(app).contains(&"section:Source:1".to_owned()));
    }

    #[test]
    fn a_reader_glob_moves_a_file_between_kind_buckets() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Kinds;
        loaded.config.classify.tests = vec!["src/**".to_owned()];
        let mut app = App::new(fixture.review(), loaded);
        app.open_working_tree_diff(None);
        assert!(
            tree_kinds(&app).contains(&"section:Tests:1".to_owned()),
            "{:?}",
            tree_kinds(&app)
        );
    }

    #[test]
    fn unmarking_in_the_review_layout_reseats_the_cursor_on_the_moved_row() {
        let fixture = standard_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
        // view everything, then unfold the viewed bucket and step onto a file
        app.handle(key('v'));
        app.handle(key('v'));
        app.handle(key('v'));
        app.handle(key('G'));
        app.handle(key('z'));
        app.handle(key('a'));
        app.handle(key('j'));
        assert_eq!(selected_path(&app), "ci.yml");
        // unmark: ci.yml jumps up into to-review; the cursor must follow it
        app.handle(key('v'));
        assert_eq!(
            tree_kinds(&app),
            [
                "section:To review:1",
                "file:ci.yml",
                "section:Viewed:2",
                "file:src/lib.rs",
                "file:todo.md",
            ]
        );
        assert_eq!(tree_cursor(&app), 1, "cursor sits on the moved file row");
    }

    #[test]
    fn marking_the_last_unviewed_file_keeps_the_cursor_in_range() {
        let fixture = standard_fixture();
        let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
        app.handle(key('v'));
        app.handle(key('v'));
        // one file left; park the cursor on the trailing viewed header
        app.handle(key('G'));
        app.handle(key('v'));
        let rows = tree_row_count(&app);
        assert_eq!(rows, 2, "both buckets, all files folded away");
        assert!(
            tree_cursor(&app) < rows,
            "cursor clamps into the shrunken row list"
        );
    }

    #[test]
    fn cycling_the_sidebar_layout_drops_a_committed_search() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.handle(key('/'));
        app.handle(key('l'));
        app.handle(key('\n'));
        assert!(app.search.is_some(), "search committed on the tree rows");
        app.handle(key('t'));
        assert!(
            app.search.is_none(),
            "layout swap invalidates the search row indices"
        );
    }

    #[test]
    fn u_jumps_to_the_next_unviewed_file_wrapping_past_viewed_ones() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.handle(key('v')); // ci.yml viewed, selection lands on todo.md
        select_file(&mut app, "todo.md");
        app.handle(key('u'));
        assert_eq!(
            selected_path(&app),
            "src/lib.rs",
            "wraps past the viewed ci.yml"
        );
    }

    #[test]
    fn u_with_everything_viewed_says_so() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        for path in ["ci.yml", "src/lib.rs", "todo.md"] {
            select_file(&mut app, path);
            app.handle(key('v'));
        }
        let before = selected_path(&app);
        app.handle(key('u'));
        assert_eq!(selected_path(&app), before);
        let message = app.message.expect("message");
        assert!(message.text.contains("every file is viewed"));
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
            Anchor {
                file: "todo.md".to_owned(),
                line: None,
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
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
                title: &format!("Review feedback: {repo} @ main (1 comment)"),
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
        app.handle(key(']'));
        assert!(matches!(
            rows(&app)[app.diff.as_ref().unwrap().cursor],
            DiffRow::Hunk { hunk: 1, .. }
        ));
        app.handle(key('['));
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
        app.settle_refresh();
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
        app.settle_refresh();
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

        let source = ReviewSource::commit(&oid);
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
            diffler_core::session::Anchor {
                file: "todo.md".into(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
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
                Anchor {
                    file: "a.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                "first\nsecond",
            )
            .id
            .clone();
        session.reply(&id, "agent", "done\nand verified");
        let lines = comment_display(&session.comments[0], u16::MAX, None);
        let plain = |s: &str| MdSpan {
            text: s.to_owned(),
            ..MdSpan::default()
        };
        assert_eq!(
            lines,
            vec![
                CommentLine::Header,
                CommentLine::Body(vec![plain("first")]),
                CommentLine::Body(vec![plain("second")]),
                CommentLine::Reply {
                    author: "agent".to_owned(),
                    spans: vec![plain("done")],
                    first: true,
                },
                CommentLine::Reply {
                    author: "agent".to_owned(),
                    spans: vec![plain("and verified")],
                    first: false,
                },
                CommentLine::Footer,
            ]
        );
    }

    #[test]
    fn comment_display_wraps_long_text_to_the_pane_width() {
        let mut session = Session::default();
        let id = session
            .add_comment(
                Anchor {
                    file: "a.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                "a body long enough that it cannot fit on one row",
            )
            .id
            .clone();
        session.reply(&id, "agent", "a reply that also runs past the pane");
        let lines = comment_display(&session.comments[0], 30, None);
        let budget = 30 - 4;
        let text = |runs: &[MdSpan]| runs.iter().map(|s| s.text.clone()).collect::<String>();
        for line in &lines {
            match line {
                CommentLine::Body(runs) => assert!(text(runs).width() <= budget, "{runs:?}"),
                CommentLine::Reply { spans, first, .. } => {
                    let head = if *first { "└ agent: ".width() } else { 2 };
                    assert!(text(spans).width() + head <= budget, "{spans:?}");
                }
                _ => {}
            }
        }
        let bodies = lines
            .iter()
            .filter(|l| matches!(l, CommentLine::Body(_)))
            .count();
        assert!(bodies > 1, "the long body wrapped onto several rows");
        // words survive the wrap intact
        let joined: Vec<String> = lines
            .iter()
            .filter_map(|l| match l {
                CommentLine::Body(runs) => Some(text(runs)),
                _ => None,
            })
            .collect();
        assert_eq!(
            joined.join(" "),
            "a body long enough that it cannot fit on one row"
        );
    }

    #[test]
    fn comment_display_hard_splits_an_unbreakable_token() {
        let mut session = Session::default();
        session.add_comment(
            Anchor {
                file: "a.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "https://example.invalid/a/very/long/unbroken/path/segment/thing",
        );
        let lines = comment_display(&session.comments[0], 24, None);
        for line in &lines {
            if let CommentLine::Body(runs) = line {
                let width: usize = runs.iter().map(|s| s.text.width()).sum();
                assert!(width <= 20, "{runs:?}");
            }
        }
    }

    #[test]
    fn deleting_a_comment_asks_for_confirmation_first() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        select_file(&mut app, "src/lib.rs");
        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('c'));
        type_text(&mut app, "why 42?");
        app.handle(key('\n'));

        let diff = app.diff.as_mut().unwrap();
        diff.ensure_rows(&app.review);
        let comment_row = rows(&app)
            .iter()
            .position(|r| matches!(r, DiffRow::Comment { .. }))
            .expect("comment row present");
        app.diff.as_mut().unwrap().cursor = comment_row;

        app.delete_comment_at_cursor();
        assert!(
            matches!(app.modal, Some(Modal::Confirm { .. })),
            "delete asks first"
        );
        assert_eq!(
            app.review.session.comments.len(),
            1,
            "nothing deleted before confirming"
        );
        app.confirm_modal();
        assert_eq!(
            app.review.session.comments.len(),
            0,
            "confirm deletes the comment"
        );
    }

    #[test]
    fn build_split_rows_aligns_old_and_new_sides() {
        let fixture = standard_fixture();
        let app = diff_app(&fixture);
        let diff = app.diff.as_ref().unwrap();
        let model = diff.model(&app.review);
        let session = app.review.session_for(&ReviewSource::WorkingTree);
        for (index, file) in model.files.iter().enumerate() {
            for row in build_split_rows(model, session, index, u16::MAX, None) {
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
    #[test]
    fn paren_jumps_land_on_definition_starts() {
        use crate::test_support::standard_fixture;
        let fixture = standard_fixture();
        fixture.write(
            "src/lib.rs",
            "pub fn answer() -> u32 {\n    42\n}\n\npub fn other() -> u32 {\n    7\n}\n",
        );
        let mut app =
            crate::app::App::new(fixture.review(), crate::config::LoadedConfig::default());
        app.open_working_tree_diff(Some("src/lib.rs"));
        let (path, content) = ("src/lib.rs".to_owned(), fixture_content(&app));
        let index = app.highlighter.scope_index(&path, &content);
        assert!(!index.is_empty(), "rust definitions indexed");
        let hash = current_hash(&app);
        if let Some(diff) = app.diff.as_mut() {
            diff.scopes
                .insert(path, crate::app::diff::FileScope { hash, index });
            diff.ensure_rows(&app.review);
            diff.focus = Pane::Diff;
        }
        app.handle(crate::test_support::key(')'));
        let diff = app.diff.as_ref().expect("diff");
        let on_def = matches!(
            diff.rows().get(diff.cursor),
            Some(DiffRow::Line { file, hunk, line })
                if app.review.model().files.get(*file)
                    .and_then(|f| f.hunks.get(*hunk))
                    .and_then(|h| h.lines.get(*line))
                    .is_some_and(|l| l.text.contains("fn "))
        );
        assert!(on_def, "cursor row {} is a definition line", diff.cursor);
    }

    /// Paths of the open review's diff, in sidebar order.
    fn against_paths(app: &App) -> Vec<String> {
        app.diff
            .as_ref()
            .and_then(|diff| diff.commit_model.as_ref())
            .expect("pinned model")
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect()
    }

    fn against_main(fixture: &Fixture) -> App {
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('d'));
        app.handle(key('d'));
        app
    }

    #[test]
    fn the_diff_transient_reviews_the_branch_against_its_base() {
        let fixture = crate::test_support::branch_fixture();
        let app = against_main(&fixture);
        let diff = app.diff.as_ref().expect("diff view");
        assert_eq!(diff.source, ReviewSource::against("main"));
        assert_eq!(app.against_rev(), Some("main"));
        // the branch commit and the uncommitted file, both
        assert_eq!(against_paths(&app), ["dirty.rs", "landed.rs"]);
        assert_eq!(app.screen(), Screen::Diff);
    }

    #[test]
    fn an_against_review_tracks_edits_through_the_refresh() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = against_main(&fixture);
        fixture.write("later.rs", "pub fn later() {}\n");
        app.queue_refresh();
        app.settle_refresh();
        assert_eq!(against_paths(&app), ["dirty.rs", "landed.rs", "later.rs"]);
    }

    #[test]
    fn a_refresh_answering_for_another_rev_leaves_the_open_review_alone() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = against_main(&fixture);
        app.apply_refresh(diffler_core::review::Refreshed {
            status: diffler_core::vcs::StatusModel::default(),
            model: DiffModel::default(),
            against: Some(("develop".to_owned(), Ok(DiffModel::default()))),
        });
        assert_eq!(against_paths(&app), ["dirty.rs", "landed.rs"]);
    }

    #[test]
    fn the_branch_picker_reviews_against_the_chosen_branch() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('d'));
        app.handle(key('b'));
        assert!(matches!(app.modal, Some(Modal::RevList { .. })));
        app.handle(code_key(crossterm::event::KeyCode::Tab));
        for c in "main".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        assert!(app.modal.is_none());
        let diff = app.diff.as_ref().expect("diff view");
        assert_eq!(diff.source, ReviewSource::against("main"));
    }

    #[test]
    fn the_commit_picker_reviews_against_the_chosen_commit() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('d'));
        app.handle(key('s'));
        let Some(Modal::RevList { entries, .. }) = &app.modal else {
            panic!("rev picker open, got {:?}", app.modal);
        };
        assert!(entries[0].label.contains("feature work"), "{entries:?}");
        // newest first, so the top entry is HEAD: only the dirty file is over it
        app.handle(key('\n'));
        assert_eq!(against_paths(&app), ["dirty.rs"]);
    }

    #[test]
    fn a_rev_that_does_not_resolve_reports_instead_of_panicking() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_against_diff("no-such-branch");
        assert!(app.diff.is_none());
        assert!(
            matches!(&app.message, Some(m) if m.severity == crate::app::Severity::Error),
            "{:?}",
            app.message
        );
    }

    #[test]
    fn w_goes_back_to_the_plain_working_tree_review() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('d'));
        app.handle(key('w'));
        let diff = app.diff.as_ref().expect("diff view");
        assert_eq!(diff.source, ReviewSource::WorkingTree);
        assert_eq!(app.against_rev(), None);
    }

    fn fixture_content(app: &crate::app::App) -> String {
        app.review
            .model()
            .files
            .iter()
            .find(|f| f.path == "src/lib.rs")
            .and_then(|f| f.new_text.clone())
            .expect("new text")
    }

    fn current_hash(app: &crate::app::App) -> String {
        app.review
            .model()
            .files
            .iter()
            .find(|f| f.path == "src/lib.rs")
            .map(diffler_core::model::FileDiff::sides_hash)
            .expect("hash")
    }
}
