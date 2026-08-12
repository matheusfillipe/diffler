//! The comments sidebar: a navigator down the review's comments that drives
//! the diff cursor. Selecting a comment seats the cursor on it, so the pane's
//! own verbs (reply, resolve, delete, yank) act on the right one with no
//! separate handling.

use super::{DiffRow, Pane};
use crate::app::App;

impl super::DiffView {
    pub fn comments_open(&self) -> bool {
        self.comments_open
    }

    pub fn comments_cursor(&self) -> usize {
        self.comments_cursor
    }
}

impl App {
    /// Comment ids of the active review, in the order the sidebar lists them:
    /// by file as the diff orders them, then by line.
    pub(crate) fn comment_order(&self) -> Vec<String> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let model = diff.model(&self.review);
        let file_rank = |path: &str| {
            model
                .files
                .iter()
                .position(|file| file.path == path)
                .unwrap_or(usize::MAX)
        };
        let session = self.review.session_for(&diff.source);
        let mut ordered: Vec<&diffler_core::session::Comment> = session.comments.iter().collect();
        ordered.sort_by_key(|comment| {
            (
                file_rank(&comment.anchor.file),
                comment.anchor.line.unwrap_or(0),
            )
        });
        ordered.iter().map(|comment| comment.id.clone()).collect()
    }

    pub(crate) fn toggle_comments_sidebar(&mut self) {
        if self.diff.is_none() {
            self.open_working_tree_diff(None);
        }
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.comments_open = !diff.comments_open;
        if !diff.comments_open {
            if diff.focus == Pane::Comments {
                diff.focus = Pane::Diff;
            }
            return;
        }
        let count = self.comment_order().len();
        // an empty sidebar is an answer, so it opens with nothing to focus
        if count == 0 {
            return;
        }
        if let Some(diff) = self.diff.as_mut() {
            diff.comments_cursor = diff.comments_cursor.min(count - 1);
            diff.focus = Pane::Comments;
        }
        self.seat_cursor_on_selected_comment();
    }

    pub(crate) fn comments_step(&mut self, delta: isize) {
        let count = self.comment_order().len();
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if count == 0 {
            return;
        }
        diff.comments_cursor = diff
            .comments_cursor
            .saturating_add_signed(delta)
            .min(count - 1);
        self.seat_cursor_on_selected_comment();
    }

    pub(crate) fn comments_to(&mut self, index: usize) {
        let count = self.comment_order().len();
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if count == 0 {
            return;
        }
        diff.comments_cursor = index.min(count - 1);
        self.seat_cursor_on_selected_comment();
    }

    /// Pull the sidebar selection back into range after the comment list
    /// shrank under it, and re-seat the diff cursor the verbs read.
    pub(crate) fn resettle_comments_cursor(&mut self) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        if !diff.comments_open {
            return;
        }
        self.comments_to(diff.comments_cursor);
    }

    /// Whether a column falls in the open comments sidebar.
    pub(crate) fn comments_col(&self, col: u16) -> bool {
        self.diff
            .as_ref()
            .is_some_and(|diff| diff.comments_open && col >= diff.comments_rect.x)
    }

    /// The comment a click lands on, through the last render's line table so
    /// a wrapped body line picks the comment it belongs to.
    pub(crate) fn comments_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let diff = self.diff.as_ref()?;
        if !diff.comments_open {
            return None;
        }
        let rect = diff.comments_rect;
        if col < rect.x || row < rect.y || row >= rect.y.saturating_add(rect.height) {
            return None;
        }
        let line = (row - rect.y) as usize + diff.comments_scroll;
        diff.comment_lines.get(line).copied().flatten()
    }

    /// Move the diff cursor onto the selected comment, switching files when it
    /// lives in another one. This is what makes the pane's verbs apply.
    pub(crate) fn seat_cursor_on_selected_comment(&mut self) {
        let order = self.comment_order();
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let Some(id) = order.get(diff.comments_cursor).cloned() else {
            return;
        };
        self.focus_comment(&id);
    }

    /// Seat the diff cursor on the comment with `id`. Reports whether the
    /// comment's file is part of this diff at all.
    pub(crate) fn focus_comment(&mut self, id: &str) -> bool {
        let Some(diff) = self.diff.as_ref() else {
            return false;
        };
        let session = self.review.session_for(&diff.source);
        let Some(file) = session
            .comments
            .iter()
            .find(|comment| comment.id == id)
            .map(|comment| comment.anchor.file.clone())
        else {
            return false;
        };
        let model = diff.model(&self.review);
        let Some(file_index) = model.files.iter().position(|entry| entry.path == file) else {
            self.info("comment file is not in this diff");
            return false;
        };
        if let Some(diff) = self.diff.as_mut()
            && diff.selected != file_index
        {
            diff.selected = file_index;
            diff.invalidate();
        }
        let Some(diff) = self.diff.as_mut() else {
            return false;
        };
        diff.reveal_selected(&self.review);
        diff.ensure_rows(&self.review);
        let Some(diff) = self.diff.as_ref() else {
            return false;
        };
        let session = self.review.session_for(&diff.source);
        let target = diff.rows().iter().position(|row| {
            matches!(row, DiffRow::Comment { comment, line: 0, .. }
                if session.comments.get(*comment).is_some_and(|c| c.id == id))
        });
        let Some(row) = target else {
            return false;
        };
        if let Some(diff) = self.diff.as_mut() {
            diff.cursor = row;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Pane;
    use crate::config::LoadedConfig;
    use crate::keymap::Action;
    use crate::test_support::{Fixture, key, standard_fixture};
    use diffler_core::session::Anchor;

    fn anchor(file: &str, line: u32) -> Anchor {
        Anchor {
            file: file.to_owned(),
            line: Some(line),
            line_end: None,
            on_old_side: false,
            line_text: None,
        }
    }

    /// Two comments in different files, so stepping has to switch files.
    fn app_with_comments() -> (Fixture, App) {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.review
            .session
            .add_comment(anchor("src/lib.rs", 2), "reviewer", "why 42?");
        app.review
            .session
            .add_comment(anchor("todo.md", 1), "reviewer", "stale note");
        app.open_working_tree_diff(None);
        (fixture, app)
    }

    fn tree_cursor_on_selected(app: &App) -> bool {
        let diff = app.diff.as_ref().expect("diff");
        let rows = super::super::sidebar_rows(diff, &app.review);
        matches!(
            rows.get(diff.tree_cursor).map(|row| &row.node),
            Some(crate::tree::TreeNode::File { index, .. }) if *index == diff.selected
        )
    }

    #[test]
    fn jumping_unfolds_the_directory_holding_the_comment() {
        let (_fixture, mut app) = app_with_comments();
        app.diff
            .as_mut()
            .expect("diff")
            .folded_dirs
            .insert("src".to_owned());

        app.handle(key('C'));

        let diff = app.diff.as_ref().expect("diff");
        assert!(!diff.folded_dirs.contains("src"), "the folder opened");
        assert!(tree_cursor_on_selected(&app));
    }

    #[test]
    fn jumping_unfolds_the_review_bucket_holding_the_comment() {
        let (_fixture, mut app) = app_with_comments();
        let diff = app.diff.as_mut().expect("diff");
        diff.layout = crate::config::FileLayout::Review;
        diff.bucket_folds.toggle_fold(crate::tree::Bucket::ToReview);

        app.handle(key('C'));

        assert!(
            !app.diff
                .as_ref()
                .expect("diff")
                .bucket_folds
                .is_folded(crate::tree::Bucket::ToReview),
            "the bucket opened"
        );
        assert!(tree_cursor_on_selected(&app));
    }

    #[test]
    fn jumping_leaves_the_other_layout_folds_alone() {
        let (_fixture, mut app) = app_with_comments();
        let diff = app.diff.as_mut().expect("diff");
        diff.folded_dirs.insert("src".to_owned());

        app.handle(key('C'));

        let diff = app.diff.as_ref().expect("diff");
        assert!(
            diff.bucket_folds.is_folded(crate::tree::Bucket::Viewed),
            "the review layout keeps its collapsed viewed pile"
        );
    }

    #[test]
    fn wiping_the_review_pulls_the_sidebar_selection_back_into_range() {
        let (_fixture, mut app) = app_with_comments();
        app.review
            .session
            .add_comment(anchor("todo.md", 2), "reviewer", "from the forge");
        let last = app.review.session.comments.len() - 1;
        app.review.session.comments[last].remote_id = Some("9".into());
        app.handle(key('C'));
        app.handle(key('j'));
        app.handle(key('j'));

        app.handle(key('D'));
        app.handle(key('y'));

        let diff = app.diff.as_ref().expect("diff");
        assert_eq!(app.review.session.comments.len(), 1, "the forge one stays");
        assert_eq!(diff.comments_cursor(), 0, "the selection follows it");
    }

    #[test]
    fn shift_d_from_the_sidebar_clears_the_whole_review_after_a_confirm() {
        let (_fixture, mut app) = app_with_comments();
        app.handle(key('C'));

        app.handle(key('D'));
        assert!(
            matches!(app.modal, Some(crate::app::Modal::Confirm { .. })),
            "delete-all asks first"
        );
        app.handle(key('y'));

        assert!(app.review.session.comments.is_empty());
    }

    #[test]
    fn c_opens_the_sidebar_and_seats_the_diff_cursor_on_the_first_comment() {
        let (_fixture, mut app) = app_with_comments();
        app.handle(key('C'));
        let diff = app.diff.as_ref().expect("diff");
        assert!(diff.comments_open());
        assert_eq!(diff.focus, Pane::Comments);
        assert!(matches!(
            diff.rows().get(diff.cursor),
            Some(DiffRow::Comment { line: 0, .. })
        ));
    }

    #[test]
    fn stepping_the_sidebar_follows_a_comment_into_another_file() {
        let (_fixture, mut app) = app_with_comments();
        app.handle(key('C'));
        let first = app.diff.as_ref().expect("diff").selected;
        app.handle(key('j'));
        let diff = app.diff.as_ref().expect("diff");
        assert_eq!(diff.comments_cursor(), 1);
        assert_ne!(diff.selected, first, "the pane switched to the other file");
        assert!(
            matches!(
                diff.rows().get(diff.cursor),
                Some(DiffRow::Comment { line: 0, .. })
            ),
            "the cursor lands on the comment in its own file"
        );
    }

    #[test]
    fn a_comment_verb_from_the_sidebar_acts_on_the_selected_comment() {
        let (_fixture, mut app) = app_with_comments();
        app.handle(key('C'));
        app.handle(key('j'));
        // `d` is the diff pane's own delete and reaches the selection untouched
        app.handle(key('d'));
        app.handle(key('y'));
        let bodies: Vec<&str> = app
            .review
            .session
            .comments
            .iter()
            .map(|c| c.body.as_str())
            .collect();
        assert_eq!(bodies, vec!["why 42?"], "the selected one went");
    }

    #[test]
    fn enter_leaves_the_sidebar_for_the_diff_and_c_closes_it() {
        let (_fixture, mut app) = app_with_comments();
        app.handle(key('C'));
        app.dispatch(Action::Open);
        assert_eq!(app.diff.as_ref().expect("diff").focus, Pane::Diff);
        assert!(app.diff.as_ref().expect("diff").comments_open());
        app.handle(key('C'));
        assert!(!app.diff.as_ref().expect("diff").comments_open());
    }

    #[test]
    fn h_and_l_walk_files_diff_comments_and_stop_at_the_ends() {
        let (_fixture, mut app) = app_with_comments();
        app.handle(key('C'));
        app.handle(key('h'));
        assert_eq!(app.diff.as_ref().expect("diff").focus, Pane::Diff);
        app.handle(key('h'));
        assert_eq!(app.diff.as_ref().expect("diff").focus, Pane::List);
        app.handle(key('h'));
        assert_eq!(app.diff.as_ref().expect("diff").focus, Pane::List);
        app.handle(key('l'));
        assert_eq!(app.diff.as_ref().expect("diff").focus, Pane::Diff);
        app.handle(key('l'));
        assert_eq!(app.diff.as_ref().expect("diff").focus, Pane::Comments);
    }

    #[test]
    fn opening_with_no_comments_shows_the_empty_sidebar() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let before = app.diff.as_ref().expect("diff").focus;

        app.handle(key('C'));

        let diff = app.diff.as_ref().expect("diff");
        assert!(diff.comments_open(), "the empty sidebar still opens");
        assert_eq!(diff.focus, before, "nothing to select, so focus holds");
    }
}
