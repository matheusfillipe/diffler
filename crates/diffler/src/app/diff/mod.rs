//! Diff/review screen state and handlers: a file sidebar listing every file
//! in the diff, and a pane showing only the selected file's hunks, lines, and
//! inline comments, flattened into a row list so the renderer only ever
//! materializes the visible slice.

mod comments;
mod focus;
mod folds;
pub(super) mod lens;
mod nav;
mod open;
mod review;
mod rowref;
mod rows;
mod sidebar;
mod slide;

pub(crate) use focus::{PendingFocus, TYPING_REFUSAL};

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

use diffler_core::classify::{Kind, Rules};
use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffModel, FileDiff, LineKind};
use diffler_core::review::Review;
use diffler_core::source::ReviewSource;
use diffler_core::syntax::ScopeIndex;
use diffler_core::walkthrough::Located;

use super::composer::{Composer, ComposerKind};
use super::rowsel::{RowSelect, RowText};
pub use comments::{CommentFacts, CommentGrouping, CommentPaneRow, group_comment_rows};
pub(crate) use rowref::RowPositions;
pub use rows::{
    CommentLine, DiffRow, REPLY_LANE, RowCopy, SplitRow, SplitSide, blocks_of, comment_display,
    folded_replies_text, summary_display,
};
use rows::{build_rows, build_split_rows};
pub use sidebar::DeclaredRequest;
use sidebar::{
    BucketFolds, next_unviewed_index, row_file_index, sidebar_rows, step_file_row,
    tree_position_of_file,
};
#[cfg(test)]
pub(crate) use slide::merge_count;
pub(crate) use slide::{Slide, stop_row_offset};

use crate::app::walkthrough::FigureCache;
use crate::config::FileLayout;
use crate::tree::Bucket;

/// Which pane has the keyboard: the file sidebar or the diff body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    List,
    Diff,
    /// The comments sidebar on the right, open only while `comments_open`.
    Comments,
    /// The symbol lens's references sidebar, in the comments sidebar's place
    /// while a symbol is focused.
    References,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAlign {
    Center,
    Top,
    Bottom,
    /// The cursor's line this many screen lines below the top.
    Offset(usize),
}

/// Per-line syntax spans for both sides of one file, keyed by the
/// both-sides content hash so an edit to either side invalidates it.
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
    /// Sidebar layout, pinned at open from `ui.diff_file_layout`.
    pub(crate) layout: FileLayout,
    /// Index into `model.files`: the file shown in the diff pane. Derived from
    /// the file under `tree_cursor` whenever that lands on a File row.
    pub selected: usize,
    /// Folded directory paths in the sidebar tree; persists across refresh.
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
    /// Rows a walkthrough stop's anchor covers, banded in the pane.
    pub(crate) referenced: Option<(usize, usize)>,
    /// The slide the walkthrough layout is windowed to; `None` means the
    /// first stop.
    pub(crate) slide: Option<Slide>,
    /// Id and time of this source's walkthrough as the open rows were built
    /// for it, so we notice an agent revising it while it is open.
    pub(crate) walkthrough_built: Option<(String, u64)>,
    /// Parsed bodies of the comments that hold a figure, so we parse a
    /// diagram only when it changes.
    pub(crate) figures: FigureCache,
    /// A figure was parsed afresh, so its `click` targets still need reading.
    figures_dirty: bool,
    /// Stop and note comments whose `anchor_ref` fails to resolve, and why.
    /// Only the worker can tell a missing file from a missing symbol or line.
    pub(crate) unresolved_anchors: HashMap<String, Located>,
    /// The walkthrough's pinned rev fails to resolve, so the worker read every
    /// file from the worktree.
    pub(crate) pin_broken: bool,
    /// First visible row of the diff pane; the renderer keeps the cursor in
    /// view.
    pub scroll: usize,
    /// Applied once the renderer knows the wrapped row heights, then cleared.
    pub(crate) scroll_align: Option<ScrollAlign>,
    /// How many screen lines below the top the cursor's line sat in the last
    /// frame, so a rebuild can put it back on the same screen row.
    pub(crate) cursor_offset: usize,
    /// Comment threads the reader opened, by comment id: an open thread shows
    /// every reply in full.
    pub(crate) open_threads: HashSet<String>,
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
    /// last render's rect and scroll for hit-testing.
    pub(crate) comments_open: bool,
    pub(crate) comments_cursor: usize,
    pub(crate) comments_scroll: usize,
    pub(crate) comments_rect: ratatui::layout::Rect,
    /// Last render's comment-sidebar line -> row index (into
    /// `App::comment_rows`), so a click on any wrapped body line selects the
    /// row it belongs to, header or comment alike.
    pub(crate) comment_lines: Vec<Option<usize>>,
    /// Comments-pane grouping cycled by `t` while it holds focus, independent
    /// of the file sidebar's own layout.
    pub(crate) comment_grouping: CommentGrouping,
    /// Folded comments-pane group keys, namespaced per grouping (`file:`,
    /// `author:`, `status:`) so each grouping keeps its own folds.
    pub(crate) comment_folds: BTreeSet<String>,
    /// Row where `V` started; `Some` means line selection is active.
    pub visual_anchor: Option<usize>,
    /// Body height of the last diff-pane render, drives half-page motions.
    pub(crate) viewport: u16,
    /// The open in-place comment editor. It takes the diff pane's keys while
    /// open.
    pub(crate) composer: Option<Composer>,
    /// Drafts the reader clicked away from, each brought back when they open
    /// the composer on the same line, reply or edit again.
    pub(crate) parked_drafts: Vec<Composer>,
    /// Rows for the selected file only.
    pub(crate) rows: Vec<DiffRow>,
    /// What a visual selection yanks for each of `rows`, same length, built
    /// alongside it. A figure entry starts as a lookup key; the render pass
    /// that draws the same figure patches its box-drawing text in once it runs.
    pub(crate) row_copy: Vec<RowCopy>,
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
    /// Paths already enriched, cleared when a refresh rebuilds the model.
    enriched: HashSet<String>,
    /// Per-file diff context override (path -> git context lines, `u32::MAX`
    /// for the whole file). Absent means the source's default context.
    pub(crate) context: HashMap<String, u32>,
    /// One `Unchanged` file per stop or note the active walkthrough anchors
    /// outside the diff, filled in by the anchor worker. Only the walkthrough
    /// layout reads it.
    pub(crate) context_files: Vec<FileDiff>,
    /// What [`Self::ensure_rows`] last merged `context_files` into, `None`
    /// when the base model covers the layout, so the render path skips the
    /// merge every frame. `on_enriched` copies enriched hunks in here too,
    /// since the merge is a snapshot taken before enrichment.
    pub(crate) merged_model: Option<DiffModel>,
    /// The image file preview the pane draws, once the worker built it.
    pub(crate) image_preview: Option<crate::app::image::ImagePreview>,
    /// The image the last draw showed, which `App::queue_image_preview`
    /// turns into a request.
    pub(crate) image_want: Option<crate::app::image::ImageKey>,
    /// The symbol lens `*` opened, until `esc` or a refresh that moves the
    /// model drops it.
    pub(crate) lens: Option<lens::Lens>,
    /// The token of the lens this view asked for and has not been answered.
    pub(crate) lens_wanted: Option<u64>,
    /// The reference each line of the references sidebar belongs to, from
    /// the last render, so a click picks the reference it lands in.
    pub(crate) ref_lines: Vec<Option<usize>>,
    /// What each unified `DiffRow::Fold` row stands for.
    pub(crate) fold_groups: Vec<folds::FoldGroup>,
    /// The hunks the reader folded, by file path then [`folds::hunk_key`].
    folded: HashMap<String, HashSet<String>>,
    /// The selected file's path as of the last row rebuild, so a fold verb
    /// can key into `folded` without needing the review in hand.
    rows_path: String,
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
            parked_drafts: Vec::new(),
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
            referenced: None,
            slide: None,
            walkthrough_built: None,
            figures: FigureCache::new(),
            figures_dirty: false,
            unresolved_anchors: HashMap::new(),
            pin_broken: false,
            scroll: 0,
            scroll_align: None,
            cursor_offset: 0,
            open_threads: HashSet::new(),
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
            comment_grouping: CommentGrouping::Flat,
            comment_folds: BTreeSet::from([comments::RESOLVED_FOLD_KEY.to_owned()]),
            visual_anchor: None,
            viewport: 0,
            rows: Vec::new(),
            row_copy: Vec::new(),
            line_rows: Vec::new(),
            rows_dirty: true,
            wrap_width: u16::MAX,
            wrap_dirty: false,
            split_rows: Vec::new(),
            highlights: HashMap::new(),
            scopes: HashMap::new(),
            enriched: HashSet::new(),
            context: HashMap::new(),
            context_files: Vec::new(),
            merged_model: None,
            image_preview: None,
            image_want: None,
            lens: None,
            lens_wanted: None,
            ref_lines: Vec::new(),
            fold_groups: Vec::new(),
            folded: HashMap::new(),
            rows_path: String::new(),
        };
        view.ensure_rows(review);
        view
    }

    pub fn model<'a>(&'a self, review: &'a Review) -> &'a DiffModel {
        self.commit_model.as_ref().unwrap_or_else(|| review.model())
    }

    /// The model every row index resolves against for `layout`: `base` plus
    /// `context_files` in the walkthrough layout, `base` alone elsewhere.
    /// Free-standing, like `model_with_context`, so a caller's other field
    /// borrows stay disjoint from the model it holds.
    pub(crate) fn model_for_layout<'a>(
        layout: FileLayout,
        base: &'a DiffModel,
        context_files: &'a [FileDiff],
    ) -> Cow<'a, DiffModel> {
        let context: &[FileDiff] = if layout == FileLayout::Walkthrough {
            context_files
        } else {
            &[]
        };
        Self::model_with_context(base, context)
    }

    /// What the last [`Self::ensure_rows`] merged, else `base`. Free-standing
    /// because the render path mutates other fields of the view while this
    /// borrow is alive.
    pub(crate) fn rendered_model<'a>(
        cached: Option<&'a DiffModel>,
        base: &'a DiffModel,
    ) -> &'a DiffModel {
        cached.unwrap_or(base)
    }

    /// [`Self::model_for_layout`] for a caller that only reads, since it
    /// borrows all of `self`.
    pub(crate) fn model_for_rows<'a>(&'a self, review: &'a Review) -> Cow<'a, DiffModel> {
        Self::model_for_layout(self.layout, self.model(review), &self.context_files)
    }

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
            // the summary card is unified-only: split_rows never carries one
            DiffRow::Summary { .. } => (0, None),
            DiffRow::Fold { group, .. } => {
                let hunk = self.fold_groups.get(group).map(|g| g.hunk);
                (
                    split
                        .iter()
                        .position(|r| matches!(r, SplitRow::Fold { hunk: h } if Some(*h) == hunk))
                        .unwrap_or(0),
                    None,
                )
            }
        }
    }

    /// Path of the selected file, when the diff is non-empty.
    pub fn selected_path(&self, review: &Review) -> Option<String> {
        self.model_for_rows(review)
            .files
            .get(self.selected)
            .map(|f| f.path.clone())
    }

    /// Mark the row list stale so the next `ensure_rows` rebuilds it. The
    /// rebuild finds the cursor, visual anchor and band again by what they
    /// sit on. Enrichment caches survive.
    pub(crate) fn mark_rows_dirty(&mut self) {
        self.rows_dirty = true;
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
        let session = review.session_for(&self.source);
        self.ensure_figures(session);
        if self.layout == FileLayout::Walkthrough {
            self.validate_slide(session);
        }
        let base = self.commit_model.as_ref().unwrap_or_else(|| review.model());
        let model_cow = Self::model_for_layout(self.layout, base, &self.context_files);
        let model: &DiffModel = &model_cow;
        self.selected = self.selected.min(model.files.len().saturating_sub(1));
        let composer = self.composer.as_ref();
        // we capture what the cursor, visual anchor and band sit on while
        // `self.rows` still holds the list they were seated against
        let positions = self.capture_positions(review);
        let (rows, copy) = build_rows(
            model,
            session,
            self.selected,
            self.wrap_width,
            composer,
            self.card_views(),
        );
        let (rows, copy) = if self.layout == FileLayout::Walkthrough {
            self.window_slide(model, session, rows, copy)
        } else {
            (rows, copy)
        };
        let path = model
            .files
            .get(self.selected)
            .map(|f| f.path.clone())
            .unwrap_or_default();
        let none_folded = HashSet::new();
        let folded = self.folded.get(&path).unwrap_or(&none_folded);
        let (rows, copy, fold_groups) = folds::apply(&rows, &copy, model, folded);
        self.rows = rows;
        self.row_copy = copy;
        let split_rows = build_split_rows(
            model,
            session,
            self.selected,
            self.wrap_width,
            composer,
            self.card_views(),
        );
        self.split_rows = folds::apply_split(split_rows, model.files.get(self.selected), folded);
        self.fold_groups = fold_groups;
        self.rows_path = path;
        // a refresh or a fold can move the pane's file in the sidebar, so we
        // reseat the tree cursor; a pure wrap re-flow must leave it alone. We
        // read this before `merged_model` below ends `model`'s borrow.
        let tree_rows = self.rows_dirty.then(|| self.tree_rows(model, session));
        // we keep the merge so the render path skips rebuilding it every frame
        self.merged_model = match model_cow {
            Cow::Borrowed(_) => None,
            Cow::Owned(model) => Some(model),
        };
        self.restore_positions(review, positions);
        if let Some(tree_rows) = tree_rows {
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
        let model = self.model_for_rows(review);
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
            // a stop list hides nothing: every stop is on screen
            FileLayout::Walkthrough => false,
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
        self.referenced = None;
        self.rows_dirty = true;
        self.ensure_rows(review);
    }

    /// Select `file_index`, reveal it in the sidebar and rebuild the pane
    /// rows, then seat the cursor on the first row `matches` picks out.
    /// Returns the matched span, first row to last, so a caller can band it.
    pub(crate) fn seat_on(
        &mut self,
        review: &Review,
        file_index: usize,
        matches: impl Fn(&DiffRow) -> bool,
    ) -> Option<(usize, usize)> {
        self.select(file_index, review);
        self.reveal_selected(review);
        self.ensure_rows(review);
        let first = self.rows.iter().position(&matches)?;
        let last = self.rows.iter().rposition(&matches).unwrap_or(first);
        self.cursor = first;
        Some((first, last))
    }

    /// The rows in `file_index` whose new-side line numbers fall in
    /// `line..=end`, first to last, so a caller can band the segment a comment
    /// speaks about.
    pub(crate) fn span_rows(
        &self,
        review: &Review,
        file_index: usize,
        line: u32,
        end: u32,
    ) -> Option<(usize, usize)> {
        let model = self.model_for_rows(review);
        let hunks = &model.files.get(file_index)?.hunks;
        let covered = |row: &DiffRow| {
            let DiffRow::Line { hunk, line: at, .. } = *row else {
                return false;
            };
            hunks
                .get(hunk)
                .and_then(|hunk| hunk.lines.get(at))
                .and_then(|diff_line| diff_line.new_no)
                .is_some_and(|no| line <= no && no <= end)
        };
        let first = self.rows.iter().position(covered)?;
        let last = self.rows.iter().rposition(covered).unwrap_or(first);
        Some((first, last))
    }

    /// The file and (hunk, line) of `path`'s line `number` on the named side.
    pub(crate) fn locate(
        &self,
        review: &Review,
        path: &str,
        on_old_side: bool,
        number: u32,
    ) -> Option<(usize, (usize, usize))> {
        let model = self.model_for_rows(review);
        let file_at = model.files.iter().position(|file| file.path == path)?;
        let file = model.files.get(file_at)?;
        file.hunks.iter().enumerate().find_map(|(hunk_at, hunk)| {
            hunk.lines
                .iter()
                .position(|line| {
                    (line.kind == LineKind::Deleted) == on_old_side
                        && line.number_on(on_old_side) == Some(number)
                })
                .map(|line_at| (file_at, (hunk_at, line_at)))
        })
    }

    /// Seat the cursor on `path`'s line `number` on the named side, moving to
    /// its file and opening the fold that hides it. Returns the file's index.
    pub(crate) fn seat_line(
        &mut self,
        review: &Review,
        path: &str,
        on_old_side: bool,
        number: u32,
    ) -> Option<usize> {
        let (file, at) = self.locate(review, path, on_old_side, number)?;
        self.select(file, review);
        self.reveal_selected(review);
        self.reveal_line(review, at);
        Some(file)
    }

    /// Whether the rows on screen are `path`'s.
    pub(crate) fn rows_show(&self, path: &str) -> bool {
        self.rows_path == path
    }

    /// Seat the cursor on line `line` of hunk `hunk`, opening the fold that
    /// hides it first. Every motion that can land inside a fold goes through
    /// here, so its target is on screen. Returns the row it landed on.
    pub(crate) fn reveal_line(
        &mut self,
        review: &Review,
        (hunk, line): (usize, usize),
    ) -> Option<usize> {
        let hiding = self
            .fold_groups
            .iter()
            .position(|group| group.lines.contains(&(hunk, line)));
        if let Some(group) = hiding {
            self.open_fold_group(group);
            self.ensure_rows(review);
        }
        let at = self.row_of_line((hunk, line))?;
        self.cursor = at;
        Some(at)
    }

    /// Rebuild the rows after this view's hunks changed, from `positions`
    /// captured before the change, and keep the cursor's line on the screen
    /// row it sat on.
    pub(crate) fn rebuild_in_place(&mut self, review: &Review, positions: RowPositions) {
        self.mark_rows_dirty();
        self.ensure_rows(review);
        self.restore_positions(review, positions);
        self.scroll_align = Some(ScrollAlign::Offset(self.cursor_offset));
    }

    /// The unified pane row under a screen point, through the line table the
    /// last frame recorded. `None` in split mode, whose paired rows don't map
    /// one to one.
    pub(crate) fn row_at_point(&self, col: u16, row: u16) -> Option<usize> {
        if self.side_by_side {
            return None;
        }
        let pane = self.pane;
        let inside = col >= pane.x
            && col < pane.x + pane.width
            && row >= pane.y
            && row < pane.y + pane.height;
        if !inside {
            return None;
        }
        self.line_rows
            .get((row - pane.y) as usize)
            .copied()
            .flatten()
    }

    pub(crate) fn card_views(&self) -> rows::CardViews<'_> {
        rows::CardViews {
            figures: &self.figures,
            unresolved_anchors: &self.unresolved_anchors,
            open_threads: &self.open_threads,
        }
    }

    /// The row showing the model line (hunk, line), or the fold row hiding it.
    pub(crate) fn row_of_line(&self, (hunk, line): (usize, usize)) -> Option<usize> {
        let shown = self.rows.iter().position(
            |row| matches!(*row, DiffRow::Line { hunk: h, line: l, .. } if h == hunk && l == line),
        );
        shown.or_else(|| {
            let group = self
                .fold_groups
                .iter()
                .position(|group| group.lines.contains(&(hunk, line)))?;
            self.rows
                .iter()
                .position(|row| matches!(*row, DiffRow::Fold { group: g, .. } if g == group))
        })
    }

    /// The text a `/` search matches against: a diff line's own text, or a
    /// fold row's hidden lines. `None` for a row search does not reach (a
    /// hunk header, a comment, the composer).
    pub(crate) fn row_search_text(&self, file: Option<&FileDiff>, row: &DiffRow) -> Option<String> {
        match *row {
            DiffRow::Line { hunk, line, .. } => {
                Some(file?.hunks.get(hunk)?.lines.get(line)?.text.clone())
            }
            DiffRow::Fold { group, .. } => Some(self.fold_text(file?, group)),
            _ => None,
        }
    }
}

impl RowSelect for DiffView {
    fn cursor(&self) -> usize {
        self.cursor
    }

    fn anchor(&self) -> Option<usize> {
        self.visual_anchor
    }

    fn set_anchor(&mut self, anchor: Option<usize>) {
        self.visual_anchor = anchor;
    }
}

impl RowText for DiffView {
    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn row_text(&self, row: usize) -> String {
        self.row_copy
            .get(row)
            .map(RowCopy::text)
            .unwrap_or_default()
    }
}

/// A queued re-diff of the working tree, plus whatever source is open (a
/// three-dot review or a pinned commit/range/PR), all recomputed on one
/// fresh backend under the algorithm just switched to (`<c-a>`).
#[derive(Debug)]
pub struct RediffRequest {
    /// The open diff's resolved source (a walkthrough already resolved to
    /// whatever it is about); `None` when no diff is open.
    pub about: Option<ReviewSource>,
    /// The PR's `(merge_base, head)` when `about` is one, so a landing result
    /// for a head that has since moved is dropped.
    pub pr_head: Option<(String, String)>,
    /// The request this answers; a stale one is dropped on arrival.
    pub token: u64,
}

impl RediffRequest {
    /// The re-diff itself, on a fresh backend: status, the working tree, and
    /// `about`'s own model when it is a three-dot review or a pinned source.
    pub fn run(
        &self,
        repo_root: &std::path::Path,
        settings: &diffler_core::diffalgo::DiffSettings,
    ) -> Result<diffler_core::review::Refreshed, String> {
        let against = match &self.about {
            Some(ReviewSource::Against { rev }) => Some(rev.as_str()),
            _ => None,
        };
        let pr_head = self
            .pr_head
            .as_ref()
            .map(|(base, head)| (base.as_str(), head.as_str()));
        let pinned = self
            .about
            .as_ref()
            .filter(|about| {
                matches!(
                    about,
                    ReviewSource::Commit { .. }
                        | ReviewSource::Range { .. }
                        | ReviewSource::Pr { .. }
                )
            })
            .map(|source| (source, pr_head));
        Review::compute_refresh(repo_root, settings, against, pinned).map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests;
