//! Moving around the diff screen: routing its actions, the mouse, and the
//! cursor across the sidebar tree and the pane's rows.

use diffler_core::source::ReviewSource;

use super::{DiffRow, DiffView, Pane, ScrollAlign, next_unviewed_index, sidebar_rows};
use crate::app::rowsel::RowSelect;
use crate::app::{App, MouseGesture, hit_index, page_step};
use crate::config::FileLayout;
use crate::keymap::Action;
use crate::tree::{TreeNode, TreeRow};

/// The row `<tab>` folds when the cursor sits on `at`: that row when it is a
/// header, otherwise the header it sits under.
fn foldable_at(rows: &[TreeRow], at: usize) -> Option<usize> {
    let row = rows.get(at)?;
    let header = TreeNode::is_group;
    if header(&row.node) {
        return Some(at);
    }
    rows.get(..at)?
        .iter()
        .rposition(|above| above.depth < row.depth && header(&above.node))
}

/// What `d` deletes from the walkthrough layout's sidebar: everything, or one
/// stop.
enum WalkthroughSidebarRow {
    Summary,
    Stop(usize),
}

impl App {
    pub(crate) fn dispatch_diff(&mut self, action: Action) {
        // a file or focus change, or a fold opening or closing, moves search
        // onto different rows, so drop it
        let scope = |diff: &DiffView| (diff.selected, diff.focus, diff.rows.len());
        let before = self.diff.as_ref().map(scope);
        self.dispatch_diff_inner(action);
        if self.search.is_some() && self.diff.as_ref().map(scope) != before {
            self.search = None;
        }
    }

    fn dispatch_diff_inner(&mut self, action: Action) {
        if let Some(diff) = self.diff.as_mut() {
            diff.ensure_rows(&self.review);
        } else {
            return;
        }
        // a file switch works from either pane and follows the sidebar order
        match action {
            Action::NextFile => return self.diff_step_file(true),
            Action::PrevFile => return self.diff_step_file(false),
            Action::NextUnviewed => {
                if self
                    .diff
                    .as_ref()
                    .is_some_and(|diff| diff.layout == FileLayout::Walkthrough)
                {
                    return self.walkthrough_jump_unseen();
                }
                return self.diff_jump_unviewed();
            }
            // each list regroups only while it has the keyboard
            Action::CycleSidebarMode => {
                match self.diff.as_ref().map(|d| d.focus) {
                    Some(Pane::Comments) => self.cycle_comment_grouping(),
                    Some(Pane::List) => self.diff_cycle_sidebar_mode(),
                    _ => self.info("move into the file list to change how it groups files"),
                }
                return;
            }
            Action::MoveLeft => return self.diff_focus(self.pane_left()),
            Action::MoveRight => return self.diff_focus(self.pane_right()),
            Action::ToggleSideBySide => return self.toggle_side_by_side(),
            Action::SubmitReview => return self.submit_pr_review(),
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
            Some(Pane::Comments) => self.dispatch_comments(action),
            Some(Pane::References) => self.dispatch_references(action),
            None => {}
        }
    }

    /// The references sidebar. Moving its selection seats the diff on that
    /// use, so the diff pane's own verbs reach it.
    fn dispatch_references(&mut self, action: Action) {
        match action {
            Action::MoveDown => self.refs_step(1, false),
            Action::MoveUp => self.refs_step(-1, false),
            Action::GoTop => self.refs_to(0),
            Action::GoBottom => self.refs_to(usize::MAX),
            Action::HalfPageDown => self.refs_step(self.refs_page(false), false),
            Action::HalfPageUp => self.refs_step(-self.refs_page(false), false),
            Action::FullPageDown => self.refs_step(self.refs_page(true), false),
            Action::FullPageUp => self.refs_step(-self.refs_page(true), false),
            Action::NextHunk => self.refs_jump_file(true),
            Action::PrevHunk => self.refs_jump_file(false),
            Action::Open => {
                self.seat_ref();
                self.diff_focus(Pane::Diff);
            }
            Action::MoveRight | Action::MoveLeft => self.diff_focus(Pane::Diff),
            other => self.dispatch_diff_pane(other),
        }
    }

    /// Panes left to right: files, diff, comments when it is open. `h` and
    /// `l` walk that order and stop at the ends.
    fn pane_left(&self) -> Pane {
        match self.diff.as_ref().map(|diff| diff.focus) {
            Some(Pane::Comments | Pane::References) => Pane::Diff,
            _ => Pane::List,
        }
    }

    fn pane_right(&self) -> Pane {
        let Some(diff) = self.diff.as_ref() else {
            return Pane::Diff;
        };
        match diff.focus {
            Pane::Diff | Pane::References if diff.refs_visible() => Pane::References,
            Pane::Diff | Pane::Comments if diff.comments_open => Pane::Comments,
            _ => Pane::Diff,
        }
    }

    /// The comments sidebar. Its selection seats the diff cursor on the
    /// comment, so the diff pane's comment verbs work here. A header or an
    /// orphan seats nothing: delete and claim address the selection by id,
    /// and everything else declines.
    fn dispatch_comments(&mut self, action: Action) {
        if Self::needs_a_selected_comment(action) {
            match self.selected_comment_id() {
                None => return self.info("no comment selected"),
                Some(id) if self.comment_is_orphan(&id) => {
                    return self.info("that comment's file is not in this diff");
                }
                Some(_) => {}
            }
        }
        match action {
            Action::MoveDown => self.comments_step(1),
            Action::MoveUp => self.comments_step(-1),
            Action::GoTop => self.comments_to(0),
            Action::GoBottom => self.comments_to(usize::MAX),
            Action::HalfPageDown => self.comments_step(self.comments_page(false)),
            Action::HalfPageUp => self.comments_step(-self.comments_page(false)),
            Action::FullPageDown => self.comments_step(self.comments_page(true)),
            Action::FullPageUp => self.comments_step(-self.comments_page(true)),
            // a flat list has no headers, so these do nothing there
            Action::NextHunk => self.comments_jump_header(true),
            Action::PrevHunk => self.comments_jump_header(false),
            Action::ToggleFold => self.comments_toggle_fold(),
            // the diff cursor may have moved since the selection seated it, so
            // `<cr>` seats it again
            Action::Open => {
                self.seat_cursor_on_selected_comment();
                self.diff_focus(Pane::Diff);
            }
            Action::MoveRight | Action::MoveLeft => self.diff_focus(Pane::Diff),
            Action::DeleteComment => self.delete_selected_comment(),
            Action::ClaimComment => self.claim_selected_comment(),
            other => self.dispatch_diff_pane(other),
        }
    }

    /// Verbs that read the diff cursor or the selected file, which decline on
    /// a header or an orphan since those seat neither.
    fn needs_a_selected_comment(action: Action) -> bool {
        matches!(
            action,
            Action::Reply
                | Action::Resolve
                | Action::Comment
                | Action::VisualSelect
                | Action::MarkViewed
                | Action::CopyFileFeedback
                | Action::OpenEditor
        )
    }

    fn dispatch_diff_list(&mut self, action: Action) {
        match action {
            Action::MoveDown => self.diff_tree_step(1),
            Action::MoveUp => self.diff_tree_step(-1),
            Action::GoTop => self.diff_tree_to(0),
            Action::GoBottom => self.diff_tree_to(usize::MAX),
            Action::NextHunk => self.diff_tree_jump(true),
            Action::PrevHunk => self.diff_tree_jump(false),
            Action::HalfPageDown => self.diff_tree_step(self.tree_page(false)),
            Action::HalfPageUp => self.diff_tree_step(-self.tree_page(false)),
            Action::FullPageDown => self.diff_tree_step(self.tree_page(true)),
            Action::FullPageUp => self.diff_tree_step(-self.tree_page(true)),
            Action::Open => self.diff_tree_activate(),
            Action::ToggleFold => self.diff_toggle_dir_fold(),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::UnviewAll => self.diff_unview_all(),
            Action::OpenEditor => self.editor_at_diff_cursor(),
            // the sidebar yanks the path its row names
            Action::CopyFileFeedback => self.copy_at_diff_tree_cursor(),
            Action::CopyAllFeedback => self.copy_feedback(false),
            Action::DeleteAllComments => self.delete_all_comments_start(),
            Action::ClaimAllComments => self.claim_all_comments_start(),
            // a file in the sidebar takes a whole-file comment
            Action::Comment => self.comment_on_selected_file(),
            // the walkthrough layout's summary and stop rows take `d` as their
            // own delete
            Action::DeleteComment => match self.walkthrough_row_at_tree_cursor() {
                Some(WalkthroughSidebarRow::Summary) => {
                    if let Some(id) = self.active_walkthrough().map(|w| w.id.clone()) {
                        self.confirm_delete_walkthrough(&id);
                    }
                }
                Some(WalkthroughSidebarRow::Stop(index)) => self.confirm_delete_stop(index),
                None => self.info("move into the diff to comment"),
            },
            Action::VisualSelect | Action::Reply | Action::Resolve | Action::ClaimComment => {
                self.info("move into the diff to comment");
            }
            Action::SymbolLens | Action::SymbolLensBack => {
                self.info("move into the diff to find references");
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
            Action::NextHunk => self.diff_jump(true, DiffRow::is_hunk_header),
            Action::PrevHunk => self.diff_jump(false, DiffRow::is_hunk_header),
            Action::NextFunction => self.diff_jump_function(true),
            Action::PrevFunction => self.diff_jump_function(false),
            Action::CenterCursor => self.diff_align(ScrollAlign::Center),
            Action::CursorTop => self.diff_align(ScrollAlign::Top),
            Action::CursorBottom => self.diff_align(ScrollAlign::Bottom),
            Action::ExpandContext => self.expand_context(),
            Action::CollapseContext => self.collapse_context(),
            Action::ExpandWholeFile => self.expand_whole_file(),
            Action::Open => self.open_figure_jump_or_focus_list(),
            // side-by-side is read-only; commenting and selection need the
            // unified pane
            Action::Comment
            | Action::VisualSelect
            | Action::Reply
            | Action::Resolve
            | Action::ClaimComment
                if self.diff.as_ref().is_some_and(|d| d.side_by_side) =>
            {
                self.info("switch to the unified view (|) to comment");
            }
            Action::Comment => self.comment_at_cursor(),
            Action::VisualSelect => self.toggle_visual(),
            Action::Reply => self.reply_at_cursor(),
            Action::Resolve => self.resolve_at_cursor(),
            Action::DeleteComment => self.delete_comment_at_cursor(),
            Action::DeleteAllComments => self.delete_all_comments_start(),
            Action::ClaimComment => self.claim_comment_at_cursor(),
            Action::ClaimAllComments => self.claim_all_comments_start(),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::UnviewAll => self.diff_unview_all(),
            Action::CopyFileFeedback => self.copy_file_or_selection(),
            Action::CopyAllFeedback => self.copy_feedback(false),
            Action::OpenEditor => self.editor_at_diff_cursor(),
            Action::OpenFigureGraph => self.open_figure_graph_at_cursor(),
            Action::SymbolLens => self.symbol_lens(true),
            Action::SymbolLensBack => self.symbol_lens(false),
            Action::ToggleFold => self.diff_toggle_fold(),
            Action::OpenAllFolds => self.diff_open_all_folds(),
            Action::FoldAll => self.diff_fold_all(),
            other => {
                self.info(format!("{} does nothing on this screen", other.name()));
            }
        }
    }

    fn toggle_side_by_side(&mut self) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.side_by_side = !diff.side_by_side;
        diff.split_scroll = 0;
        diff.settle_focus();
        self.info(if self.diff.as_ref().is_some_and(|d| d.side_by_side) {
            "side-by-side"
        } else {
            "unified"
        });
    }

    /// `za`/`<tab>` in the diff pane: fold the hunk the cursor is in, or open
    /// the folded hunk under it.
    fn diff_toggle_fold(&mut self) {
        let review = &self.review;
        let toggled = self
            .diff
            .as_mut()
            .is_some_and(|diff| diff.toggle_thread_at_cursor(review))
            || self
                .diff
                .as_mut()
                .is_some_and(|diff| diff.toggle_fold_at_cursor(review));
        if !toggled {
            self.info("move onto a hunk or a comment thread to fold it");
            return;
        }
        if let Some(diff) = self.diff.as_mut() {
            diff.ensure_rows(&self.review);
        }
    }

    /// `zR`: open every folded hunk of the file on screen.
    fn diff_open_all_folds(&mut self) {
        let opened = self.diff.as_mut().is_some_and(DiffView::open_all_folds);
        if let Some(diff) = self.diff.as_mut() {
            diff.ensure_rows(&self.review);
        }
        self.info(if opened {
            "opened every hunk"
        } else {
            "no hunk is folded"
        });
    }

    /// `zM`: fold every hunk of the file on screen.
    fn diff_fold_all(&mut self) {
        let review = &self.review;
        let folded = self.diff.as_mut().is_some_and(|diff| diff.fold_all(review));
        if let Some(diff) = self.diff.as_mut() {
            diff.ensure_rows(&self.review);
        }
        self.info(if folded {
            "folded every hunk"
        } else {
            "this file has no hunks to fold"
        });
    }

    fn diff_focus(&mut self, pane: Pane) {
        if let Some(diff) = self.diff.as_mut() {
            diff.focus = pane;
        }
    }

    /// `<cr>` in the diff pane: on a figure row naming a resolved node, jump
    /// to that code; anywhere else, move the keyboard to the sidebar.
    fn open_figure_jump_or_focus_list(&mut self) {
        match self.figure_jump_at_cursor() {
            Some((path, line, end)) => self.open_file(&path, Some((line, end)), false),
            None => self.diff_focus(Pane::List),
        }
    }

    fn diff_align(&mut self, align: ScrollAlign) {
        if let Some(diff) = self.diff.as_mut() {
            diff.scroll_align = Some(align);
        }
    }

    pub(crate) fn diff_mouse(&mut self, gesture: MouseGesture) {
        use MouseGesture;
        match gesture {
            MouseGesture::Scroll { col, down, .. } => {
                let delta = if down { 3 } else { -3 };
                let in_sidebar = self.diff.as_ref().is_some_and(|d| col < d.pane.x);
                let in_comments = self.comments_col(col);
                if self.refs_col(col) {
                    self.refs_step(delta, false);
                } else if in_comments {
                    self.comments_step(delta);
                } else if in_sidebar {
                    self.diff_tree_step(delta);
                } else {
                    self.diff_move(delta);
                }
            }
            MouseGesture::Press { col, row } => self.diff_press_at(col, row, true),
            MouseGesture::Select { col, row } => self.diff_press_at(col, row, false),
            MouseGesture::DoublePress { col, row } => self.diff_activate_at(col, row),
            MouseGesture::Drag { col, row } => self.diff_drag_to(col, row),
        }
    }

    /// Single-click: select the sidebar file under the pointer, or move the
    /// pane cursor to the clicked line, dropping any selection. `fold` lets a
    /// click on a folder fold it.
    fn diff_press_at(&mut self, col: u16, row: u16, fold: bool) {
        if let Some(index) = self.refs_row_at(col, row) {
            self.diff_focus(Pane::References);
            self.refs_to(index);
            return;
        }
        if let Some(index) = self.comments_row_at(col, row) {
            self.diff_focus(Pane::Comments);
            self.comments_to(index);
            return;
        }
        if let Some(index) = self.diff_sidebar_row_at(col, row) {
            self.diff_focus(Pane::List);
            self.diff_tree_to(index);
            if fold && self.tree_cursor_on_group() {
                self.diff_toggle_dir_fold();
            }
            return;
        }
        if let Some(index) = self.diff_pane_row_at(col, row) {
            self.diff_focus(Pane::Diff);
            if let Some(diff) = self.diff.as_mut() {
                diff.cursor = index;
                diff.visual_anchor = None;
            }
        }
    }

    /// Double-click: open the sidebar file / toggle its dir fold (like `<cr>`),
    /// open the clicked fold (like `za`), or add a comment on the clicked
    /// diff line (like `c`).
    fn diff_activate_at(&mut self, col: u16, row: u16) {
        if let Some(index) = self.diff_sidebar_row_at(col, row) {
            // we skip a folder because the pair's first click already folded it
            self.diff_tree_to(index);
            if !self.tree_cursor_on_group() {
                self.diff_tree_activate();
            }
            return;
        }
        if let Some(index) = self.diff_pane_row_at(col, row) {
            let mut on_fold = false;
            if let Some(diff) = self.diff.as_mut() {
                diff.cursor = index;
                diff.visual_anchor = None;
                on_fold = matches!(diff.rows.get(index), Some(DiffRow::Fold { .. }));
            }
            self.diff_focus(Pane::Diff);
            if on_fold {
                self.diff_toggle_fold();
            } else {
                self.comment_at_cursor();
            }
        }
    }

    /// Left-drag in the pane grows a visual line selection from the press point.
    fn diff_drag_to(&mut self, col: u16, row: u16) {
        if let Some(index) = self.diff_pane_row_at(col, row)
            && let Some(diff) = self.diff.as_mut()
        {
            if diff.visual_anchor.is_none() {
                diff.visual_anchor = Some(diff.cursor);
            }
            diff.cursor = index;
        }
    }

    /// Sidebar tree-row index under `(col, row)`, when the pointer is on a row.
    fn diff_sidebar_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let diff = self.diff.as_ref()?;
        let index = hit_index(diff.sidebar, diff.sidebar_scroll, col, row)?;
        (index < sidebar_rows(diff, &self.review).len()).then_some(index)
    }

    /// Unified pane row index under `(col, row)`. `None` in split mode, whose
    /// paired rows don't map 1:1: mouse line ops stay in the unified view.
    fn diff_pane_row_at(&self, col: u16, row: u16) -> Option<usize> {
        self.diff.as_ref()?.row_at_point(col, row)
    }

    /// Move the sidebar tree cursor by `delta` over the visible rows (dirs and
    /// files), then land the pane on the file under it when it is a file row.
    fn diff_tree_step(&mut self, delta: isize) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let rows = sidebar_rows(diff, &self.review);
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
    /// Motion and search both go through here, so landing on a file opens it.
    pub(crate) fn diff_tree_to(&mut self, target: usize) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let rows = sidebar_rows(diff, review);
        if rows.is_empty() {
            return;
        }
        let target = target.min(rows.len() - 1);
        diff.tree_cursor = target;
        match rows.get(target).map(|row| &row.node) {
            Some(TreeNode::File { index, .. }) => {
                let index = *index;
                diff.select(index, review);
                // select() re-seats the tree cursor onto the selected file row
                // via ensure_rows; restore the explicit target so it stays put
                diff.tree_cursor = target;
            }
            Some(TreeNode::Stop { index }) => {
                let index = *index;
                self.seat_stop(index);
                if let Some(diff) = self.diff.as_mut() {
                    diff.tree_cursor = target;
                }
            }
            Some(TreeNode::WalkthroughSummary) => {
                self.seat_summary();
                if let Some(diff) = self.diff.as_mut() {
                    diff.tree_cursor = target;
                }
            }
            Some(TreeNode::Dir { .. } | TreeNode::Section { .. }) | None => {}
        }
    }

    /// The walkthrough row under the sidebar cursor, when the layout is on
    /// screen and the cursor sits on its leading row or a stop.
    fn walkthrough_row_at_tree_cursor(&self) -> Option<WalkthroughSidebarRow> {
        let diff = self.diff.as_ref()?;
        if diff.layout != FileLayout::Walkthrough {
            return None;
        }
        let rows = sidebar_rows(diff, &self.review);
        match rows.get(diff.tree_cursor).map(|row| &row.node) {
            Some(TreeNode::WalkthroughSummary) => Some(WalkthroughSidebarRow::Summary),
            Some(TreeNode::Stop { index }) => Some(WalkthroughSidebarRow::Stop(*index)),
            _ => None,
        }
    }

    /// Selecting a stop puts the reader where the agent pointed: the comment's
    /// file, the cursor on the first row of its span. A stop whose file this
    /// diff does not carry leaves the pane alone. The window is keyed off the
    /// tree cursor, so we rebuild here even when two stops share a file and
    /// `select` would skip the rebuild.
    pub(crate) fn seat_stop(&mut self, index: usize) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let session = review.session_for(&diff.source);
        // a stop's own row sits past the summary row where the walkthrough
        // has one, else it is the sidebar's very first row
        let offset = diff
            .active_walkthrough(session)
            .map_or(0, super::stop_row_offset);
        diff.tree_cursor = index + offset;
        diff.slide = Some(super::Slide::Stop(index));
        diff.mark_rows_dirty();
        diff.referenced = None;
        let Some(id) = diff
            .active_walkthrough(session)
            .and_then(|walkthrough| walkthrough.stops.get(index))
        else {
            return;
        };
        let Some(comment) = session.comments.iter().position(|c| c.id == *id) else {
            return;
        };
        let Some(anchor) = session.comments.get(comment).map(|c| c.anchor.clone()) else {
            return;
        };
        let base = diff.model(review);
        let model = DiffView::model_for_layout(diff.layout, base, &diff.context_files);
        let Some(file) = model.files.iter().position(|f| f.path == anchor.file) else {
            return;
        };
        let Some(hunks) = model.files.get(file).map(|entry| entry.hunks.clone()) else {
            return;
        };
        let Some((line, end)) = anchor.span() else {
            diff.seat_on(
                review,
                file,
                |row| matches!(row, DiffRow::Comment { comment: c, line: 0, .. } if *c == comment),
            );
            return;
        };
        let covered = move |row: &DiffRow| {
            let DiffRow::Line { hunk, line: at, .. } = *row else {
                return false;
            };
            hunks
                .get(hunk)
                .and_then(|hunk| hunk.lines.get(at))
                .and_then(|dl| dl.new_no)
                .is_some_and(|no| line <= no && no <= end)
        };
        // a slide shows only the stop's region, so banding it would colour
        // every code row and read as a selection
        diff.seat_on(review, file, covered);
    }

    /// Selecting the leading summary row: the walkthrough's own summary, one
    /// card and no code.
    pub(crate) fn seat_summary(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.tree_cursor = 0;
        diff.slide = Some(super::Slide::Summary);
        diff.referenced = None;
        diff.mark_rows_dirty();
        diff.ensure_rows(review);
    }

    /// Whether the sidebar cursor sits on a folder or a section header.
    fn tree_cursor_on_group(&self) -> bool {
        self.diff.as_ref().is_some_and(|diff| {
            sidebar_rows(diff, &self.review)
                .get(diff.tree_cursor)
                .is_some_and(|row| row.node.is_group())
        })
    }

    /// The context menu's verbs for the pane in focus and the row under its
    /// cursor.
    pub(crate) fn diff_menu_actions(&self) -> Vec<Action> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let comment = vec![
            Action::Reply,
            Action::Resolve,
            Action::DeleteComment,
            Action::ClaimComment,
            Action::ToggleFold,
        ];
        let on_comment = matches!(diff.rows().get(diff.cursor), Some(DiffRow::Comment { .. }));
        match diff.focus {
            Pane::List => vec![
                Action::Open,
                Action::MarkViewed,
                Action::OpenEditor,
                Action::Blame,
                Action::CopyFileFeedback,
            ],
            Pane::Comments => comment,
            Pane::Diff | Pane::References if on_comment => comment,
            Pane::Diff | Pane::References if diff.visual_anchor.is_some() => {
                vec![Action::Comment, Action::CopyFileFeedback]
            }
            Pane::Diff | Pane::References => vec![
                Action::Comment,
                Action::VisualSelect,
                Action::SymbolLens,
                Action::OpenEditor,
                Action::Blame,
                Action::MarkViewed,
                Action::CopyFileFeedback,
            ],
        }
    }

    /// `<cr>` on the tree cursor: focus the diff pane on a file row, or toggle
    /// the fold on a directory or bucket row.
    fn diff_tree_activate(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let rows = sidebar_rows(diff, review);
        match rows.get(diff.tree_cursor).map(|r| &r.node) {
            Some(TreeNode::File { .. } | TreeNode::Stop { .. } | TreeNode::WalkthroughSummary) => {
                self.diff_focus(Pane::Diff);
            }
            Some(TreeNode::Dir { .. } | TreeNode::Section { .. }) => self.diff_toggle_dir_fold(),
            None => {}
        }
    }

    /// `za`/`<tab>`: toggle the fold of the group the tree cursor sits in.
    fn diff_toggle_dir_fold(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let rows = sidebar_rows(diff, review);
        let Some(target) = foldable_at(&rows, diff.tree_cursor) else {
            // a file at the repo root sits in no folder
            self.info("nothing to fold here");
            return;
        };
        match rows.get(target).map(|row| &row.node) {
            Some(TreeNode::Dir { path, .. }) => {
                let path = path.clone();
                if !diff.folded_dirs.remove(&path) {
                    diff.folded_dirs.insert(path);
                }
            }
            Some(TreeNode::Section { bucket, .. }) => diff.bucket_folds.toggle_fold(*bucket),
            _ => return,
        }
        // we seat the cursor on the row that folded, since the tree shrank
        let rows = sidebar_rows(diff, review);
        diff.tree_cursor = target.min(rows.len().saturating_sub(1));
    }

    /// `<c-n>`/`<c-p>`: jump the tree cursor to the next/prev file row (skipping
    /// directories), updating the pane's file. Keeps the current focus.
    fn diff_step_file(&mut self, forward: bool) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let rows = sidebar_rows(diff, &self.review);
        if let Some(target) = super::step_file_row(&rows, diff.tree_cursor, forward) {
            self.diff_tree_to(target);
        }
    }

    /// `u`: land on the next file not yet marked viewed, wrapping past the
    /// end, from either pane.
    fn diff_jump_unviewed(&mut self) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        if diff.model(&self.review).files.is_empty() {
            self.info("nothing to review");
            return;
        }
        match next_unviewed_index(diff, &self.review, true) {
            Some(index) => self.diff_select_file_index(index),
            None => self.info("every file is viewed"),
        }
    }

    /// `t`: cycle the sidebar layout, keeping the pane's file and re-seating
    /// the tree cursor on its row when visible.
    fn diff_cycle_sidebar_mode(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let has_walkthrough = matches!(diff.source, ReviewSource::Walkthrough { .. });
        let empty = diff
            .commit_model
            .as_ref()
            .unwrap_or_else(|| review.model())
            .files
            .is_empty();
        let leaving_walkthrough = diff.layout == FileLayout::Walkthrough;
        let layout = diff.cycle_layout(has_walkthrough, empty);
        diff.mark_rows_dirty();
        if leaving_walkthrough {
            diff.referenced = None;
        }
        let rows = sidebar_rows(diff, review);
        diff.reseat_tree_cursor(&rows);
        // arriving on the stop list seats the reader on its first row: the
        // summary where the walkthrough has one, else the first stop
        let offset = diff
            .active_walkthrough(review.session_for(&diff.source))
            .map_or(0, super::stop_row_offset);
        let arriving = (layout == FileLayout::Walkthrough).then_some(diff.tree_cursor);
        // a committed search indexes the old layout's rows
        self.search = None;
        if let Some(row) = arriving {
            match row.checked_sub(offset) {
                Some(stop) => self.seat_stop(stop),
                None => self.seat_summary(),
            }
        }
        self.queue_declared();
        self.info(format!("sidebar: {layout}"));
    }

    /// `y` in the file sidebar: the repo-relative path the row names, a file's
    /// or a folder's. A section header groups files without naming a path.
    fn copy_at_diff_tree_cursor(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let rows = sidebar_rows(diff, review);
        let path = match rows.get(diff.tree_cursor).map(|row| &row.node) {
            Some(TreeNode::Dir { path, .. }) => Some(path.clone()),
            Some(TreeNode::File { index, .. }) => diff
                .model(review)
                .files
                .get(*index)
                .map(|file| file.path.clone()),
            Some(
                TreeNode::Section { .. } | TreeNode::Stop { .. } | TreeNode::WalkthroughSummary,
            )
            | None => None,
        };
        match path {
            Some(path) => {
                self.info(format!("copied {path}"));
                self.pending_clipboard = Some(path);
            }
            None => self.info("nothing to copy here"),
        }
    }

    fn editor_at_diff_cursor(&mut self) {
        match self.diff_cursor_file_line() {
            Some((path, _)) if !self.review.repo_root.join(&path).exists() => {
                self.info(format!(
                    "{path} is not in the working tree, nothing to edit"
                ));
            }
            Some((path, line)) => self.request_editor(&path, line),
            None => self.info("no file under the cursor"),
        }
    }

    /// The file and line the diff cursor addresses, shared by the editor jump
    /// and blame so both land on the same place.
    pub(crate) fn diff_cursor_file_line(&self) -> Option<(String, Option<u32>)> {
        self.diff.as_ref().and_then(|diff| {
            let model = diff.model_for_rows(&self.review);
            if diff.focus == Pane::List {
                let file = model.files.get(diff.selected)?;
                return Some((file.path.clone(), None));
            }
            match diff.rows.get(diff.cursor) {
                Some(DiffRow::Hunk { file, .. } | DiffRow::Fold { file, .. }) => {
                    Some((model.files.get(*file)?.path.clone(), None))
                }
                Some(DiffRow::Line { file, hunk, line }) => {
                    let file = model.files.get(*file)?;
                    let line = file.hunks.get(*hunk)?.lines.get(*line)?;
                    Some((file.path.clone(), line.new_no.or(line.old_no)))
                }
                Some(DiffRow::Comment { comment, .. }) => self
                    .review
                    .session_for(&diff.source)
                    .comments
                    .get(*comment)
                    .map(|c| (c.anchor.file.clone(), c.anchor.line_end.or(c.anchor.line))),
                // neither carries a file of its own: the composer sits on
                // `diff.selected` and the summary card sits on nothing at all
                Some(DiffRow::Composer { .. } | DiffRow::Summary { .. }) | None => {
                    let file = model.files.get(diff.selected)?;
                    Some((file.path.clone(), None))
                }
            }
        })
    }

    fn diff_move(&mut self, delta: isize) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let last = diff.rows.len().saturating_sub(1);
        diff.cursor = diff.cursor.saturating_add_signed(delta).min(last);
    }

    fn diff_page(&self, full: bool) -> isize {
        let step = page_step(self.diff.as_ref().map_or(0, |d| d.viewport), full);
        isize::try_from(step).unwrap_or(20)
    }

    /// Rows of the file sidebar a paging key covers: its rows are one line
    /// each, so a page of the list is a page of the pane.
    fn tree_page(&self, full: bool) -> isize {
        let height = self.diff.as_ref().map_or(0, |diff| diff.sidebar.height);
        isize::try_from(page_step(height, full)).unwrap_or(20)
    }

    /// Comments a paging key covers: how many cards the pane holds on
    /// average, which keeps paging up and down symmetric.
    fn comments_page(&self, full: bool) -> isize {
        /// Before the first render there is no pane to measure.
        const UNMEASURED: usize = 5;
        let Some(diff) = self.diff.as_ref() else {
            return 1;
        };
        let rows = page_step(diff.comments_rect.height, full);
        let lines = diff.comment_lines.len();
        let pane_rows = self.comment_rows().len();
        let step = if lines == 0 || pane_rows == 0 {
            UNMEASURED
        } else {
            (rows * pane_rows / lines).max(1)
        };
        isize::try_from(step).unwrap_or(1)
    }

    /// Jump the pane cursor to the next/previous comment block, landing on its
    /// header row (`line == 0`). In the walkthrough layout the walk runs over
    /// the slides.
    fn diff_jump_comment(&mut self, forward: bool) {
        if self
            .diff
            .as_ref()
            .is_some_and(|diff| diff.layout == FileLayout::Walkthrough)
        {
            self.walk_slide_comments(forward);
            return;
        }
        self.diff_jump(forward, |row| {
            matches!(row, DiffRow::Comment { line: 0, .. })
        });
    }

    /// Every comment of the review in slide order: slide by slide, each one's
    /// region by line, then whatever no region holds, which the walk reaches
    /// as an ad hoc slide of its own.
    fn slide_comment_order(&self) -> Vec<String> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let session = self.review.session_for(&diff.source);
        let mut order: Vec<String> = Vec::new();
        for stop in diff
            .active_walkthrough(session)
            .into_iter()
            .flat_map(|walkthrough| &walkthrough.stops)
        {
            let Some(primary) = session.comments.iter().position(|c| c.id == *stop) else {
                continue;
            };
            for index in crate::app::walkthrough::slide_comments(session, primary) {
                let Some(id) = session.comments.get(index).map(|c| c.id.clone()) else {
                    continue;
                };
                if !order.contains(&id) {
                    order.push(id);
                }
            }
        }
        for id in self.comment_order() {
            if !order.contains(&id) {
                order.push(id);
            }
        }
        order
    }

    /// Step the slide walk one comment either way, entering the slide that
    /// holds where it lands. The summary sits before the first entry of
    /// `order`.
    fn walk_slide_comments(&mut self, forward: bool) {
        let on_summary = matches!(
            self.diff.as_ref().map(|diff| &diff.slide),
            Some(Some(super::Slide::Summary))
        );
        let order = self.slide_comment_order();
        if on_summary {
            if forward && let Some(id) = order.first().cloned() {
                self.focus_comment(&id);
            }
            return;
        }
        let here = self.comment_at_diff_cursor().or_else(|| {
            let diff = self.diff.as_ref()?;
            let session = self.review.session_for(&diff.source);
            let primary = diff.slide_primary(session)?;
            session.comments.get(primary).map(|c| c.id.clone())
        });
        let at = here.and_then(|id| order.iter().position(|known| *known == id));
        if !forward
            && at == Some(0)
            && self
                .active_walkthrough()
                .is_some_and(|w| w.summary.is_some())
        {
            self.seat_summary();
            return;
        }
        let next = match at {
            Some(at) if forward => order.get(at + 1),
            Some(at) => at.checked_sub(1).and_then(|back| order.get(back)),
            None => order.first(),
        };
        let Some(id) = next.cloned() else {
            return;
        };
        self.focus_comment(&id);
    }

    /// The comment the diff cursor stands on, when it stands on a card.
    fn comment_at_diff_cursor(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        let DiffRow::Comment { comment, .. } = diff.rows.get(diff.cursor)? else {
            return None;
        };
        self.review
            .session_for(&diff.source)
            .comments
            .get(*comment)
            .map(|c| c.id.clone())
    }

    /// Jump to the next/previous definition start in the diff, one a fold
    /// hides included, using the breadcrumb's scope index.
    fn diff_jump_function(&mut self, forward: bool) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let Some(path) = diff.selected_path(&self.review) else {
            return;
        };
        let Some(scope) = diff.scopes.get(&path) else {
            self.info("this file is still being parsed, try again in a moment");
            return;
        };
        let starts: std::collections::HashSet<u32> = scope
            .index
            .def_starts()
            .into_iter()
            .filter_map(|row| u32::try_from(row + 1).ok())
            .collect();
        if starts.is_empty() {
            self.info("no definitions in this file");
            return;
        }
        let model = diff.model_for_rows(&self.review);
        let Some(file) = model.files.get(diff.selected) else {
            return;
        };
        let target = self.diff_step_to_line(forward, |(hunk, line)| {
            file.hunks
                .get(hunk)
                .and_then(|h| h.lines.get(line))
                .and_then(|l| l.new_no)
                .is_some_and(|no| starts.contains(&no))
        });
        if let Some(target) = target
            && let Some(diff) = self.diff.as_mut()
        {
            diff.reveal_line(&self.review, target);
        }
    }

    /// The next line `is_target` accepts from the cursor, visible or inside a
    /// fold, as its `(hunk, line)`.
    fn diff_step_to_line(
        &self,
        forward: bool,
        is_target: impl Fn((usize, usize)) -> bool,
    ) -> Option<(usize, usize)> {
        let diff = self.diff.as_ref()?;
        let hidden = |group: usize| {
            let mut lines = diff.fold_groups.get(group)?.lines.iter().copied();
            if forward {
                lines.find(|&line| is_target(line))
            } else {
                lines.rfind(|&line| is_target(line))
            }
        };
        let target = |row: &DiffRow| match *row {
            DiffRow::Line { hunk, line, .. } => is_target((hunk, line)).then_some((hunk, line)),
            DiffRow::Fold { group, .. } => hidden(group),
            _ => None,
        };
        let row = crate::app::step_to(&diff.rows, diff.cursor, forward, |row| {
            target(row).is_some()
        })?;
        diff.rows.get(row).and_then(target)
    }

    fn diff_jump(&mut self, forward: bool, target: impl Fn(&DiffRow) -> bool) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if let Some(position) = crate::app::step_to(&diff.rows, diff.cursor, forward, target) {
            diff.cursor = position;
        }
    }

    /// `[`/`]` in the file sidebar: the previous/next group header.
    fn diff_tree_jump(&mut self, forward: bool) {
        let review = &self.review;
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let rows = sidebar_rows(diff, review);
        let header = |row: &crate::tree::TreeRow| {
            matches!(
                row.node,
                crate::tree::TreeNode::Dir { .. } | crate::tree::TreeNode::Section { .. }
            )
        };
        let Some(position) = crate::app::step_to(&rows, diff.tree_cursor, forward, header) else {
            return;
        };
        self.diff_tree_to(position);
    }

    /// Path of the selected file in the diff view.
    pub(crate) fn diff_cursor_path(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        diff.selected_path(&self.review)
    }

    /// After a refresh, keep the selected file by path; clamp if it is gone.
    pub(crate) fn restore_diff_cursor(&mut self, path: Option<String>) {
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

    /// `V`: start or end a selection, on any row kind.
    fn toggle_visual(&mut self) {
        if let Some(diff) = self.diff.as_mut() {
            RowSelect::toggle_visual(diff);
        }
    }
}
