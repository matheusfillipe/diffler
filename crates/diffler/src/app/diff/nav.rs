//! Moving around the diff screen: routing its actions, the mouse, and the
//! cursor across the sidebar tree and the pane's rows.

use super::{DiffRow, Pane, ScrollAlign, next_unviewed_index, sidebar_rows};
use crate::app::{App, MouseGesture, hit_index, page_step};
use crate::keymap::Action;
use crate::tree::{TreeNode, TreeRow};

/// The row `<tab>` folds when the cursor sits on `at`: that row when it is a
/// header, otherwise the header it sits under. Most rows are files, and
/// collapsing the group you are inside is what the key is for.
fn foldable_at(rows: &[TreeRow], at: usize) -> Option<usize> {
    let row = rows.get(at)?;
    let header = |node: &TreeNode| matches!(node, TreeNode::Dir { .. } | TreeNode::Section { .. });
    if header(&row.node) {
        return Some(at);
    }
    rows.get(..at)?
        .iter()
        .rposition(|above| above.depth < row.depth && header(&above.node))
}

impl App {
    pub(crate) fn dispatch_diff(&mut self, action: Action) {
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
            Action::NextUnviewed => return self.diff_jump_unviewed(),
            Action::CycleSidebarMode => return self.diff_cycle_sidebar_mode(),
            Action::MoveLeft => return self.diff_focus(self.pane_left()),
            Action::MoveRight => return self.diff_focus(self.pane_right()),
            Action::ToggleSideBySide => return self.toggle_side_by_side(),
            // comment walk works from either pane; land in the diff pane on the
            // comment so it can be read and replied to
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
            None => {}
        }
    }

    /// Panes left to right: files, diff, comments when it is open. `h` and
    /// `l` walk that order and stop at the ends.
    fn pane_left(&self) -> Pane {
        match self.diff.as_ref().map(|diff| diff.focus) {
            Some(Pane::Comments) => Pane::Diff,
            _ => Pane::List,
        }
    }

    fn pane_right(&self) -> Pane {
        let Some(diff) = self.diff.as_ref() else {
            return Pane::Diff;
        };
        match diff.focus {
            Pane::Diff | Pane::Comments if diff.comments_open => Pane::Comments,
            _ => Pane::Diff,
        }
    }

    /// The comments sidebar. Its selection drives the diff cursor onto the
    /// comment, so every comment verb (reply, resolve, delete, yank) is the
    /// pane's own and works here untouched. An orphan seats no cursor and no
    /// file: delete addresses the selection by id, and everything else
    /// declines.
    fn dispatch_comments(&mut self, action: Action) {
        match action {
            Action::MoveDown => self.comments_step(1),
            Action::MoveUp => self.comments_step(-1),
            Action::GoTop => self.comments_to(0),
            Action::GoBottom => self.comments_to(usize::MAX),
            Action::HalfPageDown => self.comments_step(self.comments_page(false)),
            Action::HalfPageUp => self.comments_step(-self.comments_page(false)),
            Action::FullPageDown => self.comments_step(self.comments_page(true)),
            Action::FullPageUp => self.comments_step(-self.comments_page(true)),
            // the cursor already sits on the comment, so entering the diff
            // is a focus move, and so is stepping out either side
            Action::Open | Action::MoveRight | Action::MoveLeft => self.diff_focus(Pane::Diff),
            Action::DeleteComment => self.delete_selected_comment(),
            // these read the diff cursor or the selected file, and an orphan
            // seats neither. The review-wide verbs need no row and stay live
            Action::Reply
            | Action::Resolve
            | Action::Comment
            | Action::VisualSelect
            | Action::MarkViewed
            | Action::CopyFileFeedback
            | Action::OpenEditor
                if self.selected_comment_is_orphan() =>
            {
                self.info("that comment's file is not in this diff");
            }
            other => self.dispatch_diff_pane(other),
        }
    }

    fn dispatch_diff_list(&mut self, action: Action) {
        match action {
            Action::MoveDown => self.diff_tree_step(1),
            Action::MoveUp => self.diff_tree_step(-1),
            Action::GoTop => self.diff_tree_to(0),
            Action::GoBottom => self.diff_tree_to(usize::MAX),
            Action::NextHunk => self.diff_tree_jump(true),
            Action::PrevHunk => self.diff_tree_jump(false),
            // the paging keys move the pane that has the keyboard, so here they
            // walk the file list by a screenful
            Action::HalfPageDown => self.diff_tree_step(self.tree_page(false)),
            Action::HalfPageUp => self.diff_tree_step(-self.tree_page(false)),
            Action::FullPageDown => self.diff_tree_step(self.tree_page(true)),
            Action::FullPageUp => self.diff_tree_step(-self.tree_page(true)),
            // <cr> focuses the pane on a file row, folds/unfolds a dir row
            Action::Open => self.diff_tree_activate(),
            Action::ToggleFold => self.diff_toggle_dir_fold(),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::UnviewAll => self.diff_unview_all(),
            Action::OpenEditor => self.editor_at_diff_cursor(),
            // copy and delete-all are file/review scoped, so the list serves them
            Action::CopyFileFeedback => self.copy_file_or_selection(),
            Action::CopyAllFeedback => self.copy_feedback(false),
            Action::DeleteAllComments => self.delete_all_comments_start(),
            // a file in the sidebar takes a whole-file comment; the line-scoped
            // actions still need the diff pane
            Action::Comment => self.comment_on_selected_file(),
            Action::VisualSelect | Action::Reply | Action::Resolve | Action::DeleteComment => {
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
            Action::NextFunction => self.diff_jump_function(true),
            Action::PrevFunction => self.diff_jump_function(false),
            Action::CenterCursor => self.diff_align(ScrollAlign::Center),
            Action::CursorTop => self.diff_align(ScrollAlign::Top),
            Action::CursorBottom => self.diff_align(ScrollAlign::Bottom),
            Action::ExpandContext => self.expand_context(),
            Action::CollapseContext => self.collapse_context(),
            Action::ExpandWholeFile => self.expand_whole_file(),
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
            Action::DeleteComment => self.delete_comment_at_cursor(),
            Action::DeleteAllComments => self.delete_all_comments_start(),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::UnviewAll => self.diff_unview_all(),
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

    fn diff_focus(&mut self, pane: Pane) {
        if let Some(diff) = self.diff.as_mut() {
            diff.focus = pane;
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
                // the sidebar fills the left columns; scroll whichever pane the
                // pointer sits over
                let in_sidebar = self.diff.as_ref().is_some_and(|d| col < d.pane.x);
                let in_comments = self.comments_col(col);
                if in_comments {
                    self.comments_step(delta);
                } else if in_sidebar {
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
        if let Some(index) = self.comments_row_at(col, row) {
            self.diff_focus(Pane::Comments);
            self.comments_to(index);
            return;
        }
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
        let index = hit_index(diff.sidebar, diff.sidebar_scroll, col, row)?;
        (index < sidebar_rows(diff, &self.review).len()).then_some(index)
    }

    /// Unified pane row index under `(col, row)`. `None` in split mode, whose
    /// paired rows don't map 1:1: mouse line ops stay in the unified view.
    fn diff_pane_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let diff = self.diff.as_ref()?;
        if diff.side_by_side {
            return None;
        }
        let pane = diff.pane;
        let inside = col >= pane.x
            && col < pane.x + pane.width
            && row >= pane.y
            && row < pane.y + pane.height;
        if !inside {
            return None;
        }
        diff.line_rows
            .get((row - pane.y) as usize)
            .copied()
            .flatten()
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
    /// Every way of reaching a row goes through here, motion and search alike,
    /// so landing on a file always opens it.
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
    /// the fold on a directory or bucket row.
    fn diff_tree_activate(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let rows = sidebar_rows(diff, review);
        match rows.get(diff.tree_cursor).map(|r| &r.node) {
            Some(TreeNode::File { .. }) => self.diff_focus(Pane::Diff),
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
            // a file at the repo root sits in no folder, and a key that does
            // nothing has to say so
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
        // the row that folded is the one to stand on, and folding past the
        // cursor shrinks the tree
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

    /// `t`: cycle the sidebar layout (tree → review), keeping the
    /// pane's file and re-seating the tree cursor on its row when visible.
    fn diff_cycle_sidebar_mode(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let layout = diff.cycle_layout();
        let rows = sidebar_rows(diff, review);
        diff.reseat_tree_cursor(&rows);
        // a committed search indexes the old layout's rows
        self.search = None;
        self.queue_declared();
        self.info(format!("sidebar: {layout}"));
    }

    /// `e`: open the selected file in the editor: at the line for diff line
    /// rows (new side, old side for deletions), at the anchor for comment
    /// rows, at the top otherwise (hunk header, or focus on the list).
    fn editor_at_diff_cursor(&mut self) {
        match self.diff_cursor_file_line() {
            Some((path, line)) => self.request_editor(&path, line),
            None => self.info("no file under the cursor"),
        }
    }

    /// The file and line the diff cursor addresses, shared by the editor jump
    /// and blame so both land on the same place.
    pub(crate) fn diff_cursor_file_line(&self) -> Option<(String, Option<u32>)> {
        self.diff.as_ref().and_then(|diff| {
            let model = diff.model(&self.review);
            let file = model.files.get(diff.selected)?;
            if diff.focus == Pane::List {
                return Some((file.path.clone(), None));
            }
            match diff.rows.get(diff.cursor) {
                Some(DiffRow::Hunk { .. } | DiffRow::Composer { .. }) | None => {
                    Some((file.path.clone(), None))
                }
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

    /// Comments a paging key covers. A card is several rows tall and they
    /// differ, so the step is how many the pane's rows hold on average, which
    /// keeps paging up and down symmetric.
    fn comments_page(&self, full: bool) -> isize {
        /// Before the first render there is no pane to measure.
        const UNMEASURED: usize = 5;
        let Some(diff) = self.diff.as_ref() else {
            return 1;
        };
        let rows = page_step(diff.comments_rect.height, full);
        let lines = diff.comment_lines.len();
        let cards = self.comment_order().len();
        let step = if lines == 0 || cards == 0 {
            UNMEASURED
        } else {
            (rows * cards / lines).max(1)
        };
        isize::try_from(step).unwrap_or(1)
    }

    /// Jump the pane cursor to the next/previous comment block, landing on its
    /// header row (`line == 0`) so multi-line comments are stepped as one.
    fn diff_jump_comment(&mut self, forward: bool) {
        self.diff_jump(forward, |row| {
            matches!(row, DiffRow::Comment { line: 0, .. })
        });
    }

    /// Jump to the next/previous definition start visible in the diff, using
    /// the tree-sitter scope index the breadcrumb already maintains.
    fn diff_jump_function(&mut self, forward: bool) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let Some(path) = diff.selected_path(&self.review) else {
            return;
        };
        let Some(scope) = diff.scopes.get(&path) else {
            self.info("no definition index for this file (yet)");
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
        let model = diff
            .commit_model
            .clone()
            .unwrap_or_else(|| self.review.model().clone());
        let file = model.files.iter().position(|f| f.path == path);
        self.diff_jump(forward, |row| {
            let DiffRow::Line {
                file: f,
                hunk,
                line,
            } = row
            else {
                return false;
            };
            Some(*f) == file
                && model
                    .files
                    .get(*f)
                    .and_then(|fd| fd.hunks.get(*hunk))
                    .and_then(|h| h.lines.get(*line))
                    .and_then(|l| l.new_no)
                    .is_some_and(|no| starts.contains(&no))
        });
    }

    fn diff_jump(&mut self, forward: bool, target: impl Fn(&DiffRow) -> bool) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if let Some(position) = crate::app::step_to(&diff.rows, diff.cursor, forward, target) {
            diff.cursor = position;
        }
    }

    /// `[`/`]` in the file sidebar: the previous/next group header, whichever
    /// the layout draws, so a long tree steps folder by folder the way the
    /// status screen steps its sections.
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
}
