//! The comments sidebar: a navigator down the review's comments that drives
//! the diff cursor. Selecting a comment seats the cursor on it, so the pane's
//! own verbs (reply, resolve, delete, yank) act on the right one with no
//! separate handling.

use std::collections::{BTreeSet, HashMap};

use diffler_core::session::CommentStatus;

use super::{DiffRow, Pane};
use crate::app::App;

/// The comments pane's four groupings, cycled by `t` while it holds focus.
/// The file sidebar's own layout (`crate::config::FileLayout`) is untouched:
/// this is a second, independent axis over the same review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentGrouping {
    File,
    Author,
    Status,
    Flat,
}

impl CommentGrouping {
    pub(crate) fn cycle(self) -> Self {
        match self {
            Self::File => Self::Author,
            Self::Author => Self::Status,
            Self::Status => Self::Flat,
            Self::Flat => Self::File,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::File => "by file",
            Self::Author => "by author",
            Self::Status => "by status",
            Self::Flat => "flat list",
        }
    }
}

/// The group key `status` grouping folds by default, the way the review
/// layout's viewed bucket starts folded: a finished thread is what the
/// reader did not come to read.
pub(crate) const RESOLVED_FOLD_KEY: &str = "status:resolved";

/// One comment's grouping-relevant facts, independent of the session so
/// rendering and cursor stepping bucket the same review the same way from
/// either side (`crate::ui::diff` builds these from its own render context;
/// `App::comment_rows` builds them from the session directly).
#[derive(Debug, Clone)]
pub struct CommentFacts {
    pub id: String,
    pub file: String,
    pub author: String,
    pub status: CommentStatus,
    pub orphan: bool,
}

/// One row of the comments pane under a grouping: a header naming its group
/// and how many comments it holds, or one comment. Mirrors the file
/// sidebar's own header/file row split (`crate::tree::TreeNode`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentPaneRow {
    Header {
        key: String,
        label: String,
        count: usize,
        folded: bool,
    },
    Item {
        id: String,
        orphan: bool,
    },
}

fn status_key(status: CommentStatus) -> &'static str {
    match status {
        CommentStatus::Open => "status:open",
        CommentStatus::Replied => "status:replied",
        CommentStatus::Resolved => RESOLVED_FOLD_KEY,
    }
}

fn status_label(status: CommentStatus) -> &'static str {
    match status {
        CommentStatus::Open => "Open",
        CommentStatus::Replied => "Replied",
        CommentStatus::Resolved => "Resolved",
    }
}

/// Bucket `items` under a header per distinct key, in the order each key
/// first appears. `items` already carries the pane's base order (by file,
/// then line), so grouping by file reads in diff order and grouping by
/// author reads in the order each author's first comment appears; a group
/// with nothing in it never gets a header.
fn group_by(
    items: &[CommentFacts],
    folds: &BTreeSet<String>,
    keyer: impl Fn(&CommentFacts) -> (String, String),
) -> Vec<CommentPaneRow> {
    let mut order: Vec<String> = Vec::new();
    let mut buckets: HashMap<String, (String, Vec<&CommentFacts>)> = HashMap::new();
    for item in items {
        let (key, label) = keyer(item);
        buckets
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key.clone());
                (label, Vec::new())
            })
            .1
            .push(item);
    }
    let mut rows = Vec::new();
    for key in order {
        let Some((label, bucket)) = buckets.remove(&key) else {
            continue;
        };
        let folded = folds.contains(&key);
        rows.push(CommentPaneRow::Header {
            key: key.clone(),
            label,
            count: bucket.len(),
            folded,
        });
        if !folded {
            rows.extend(bucket.into_iter().map(|item| CommentPaneRow::Item {
                id: item.id.clone(),
                orphan: item.orphan,
            }));
        }
    }
    rows
}

/// Status groups in a fixed order (open, replied, resolved) rather than
/// first appearance, so the pane reads the same way every time it groups by
/// status.
fn group_by_status(items: &[CommentFacts], folds: &BTreeSet<String>) -> Vec<CommentPaneRow> {
    let mut rows = Vec::new();
    for status in [
        CommentStatus::Open,
        CommentStatus::Replied,
        CommentStatus::Resolved,
    ] {
        let bucket: Vec<&CommentFacts> =
            items.iter().filter(|item| item.status == status).collect();
        if bucket.is_empty() {
            continue;
        }
        let key = status_key(status).to_owned();
        let folded = folds.contains(&key);
        rows.push(CommentPaneRow::Header {
            key: key.clone(),
            label: status_label(status).to_owned(),
            count: bucket.len(),
            folded,
        });
        if !folded {
            rows.extend(bucket.into_iter().map(|item| CommentPaneRow::Item {
                id: item.id.clone(),
                orphan: item.orphan,
            }));
        }
    }
    rows
}

/// The comments pane's rows under `grouping`: a flat list yields every
/// comment with no header at all, the other three group it the way the file
/// sidebar's review and kinds layouts group files (`DiffView::section_rows`).
pub fn group_comment_rows(
    items: &[CommentFacts],
    grouping: CommentGrouping,
    folds: &BTreeSet<String>,
) -> Vec<CommentPaneRow> {
    match grouping {
        CommentGrouping::Flat => items
            .iter()
            .map(|item| CommentPaneRow::Item {
                id: item.id.clone(),
                orphan: item.orphan,
            })
            .collect(),
        CommentGrouping::File => group_by(items, folds, |item| {
            (format!("file:{}", item.file), item.file.clone())
        }),
        CommentGrouping::Author => group_by(items, folds, |item| {
            (format!("author:{}", item.author), item.author.clone())
        }),
        CommentGrouping::Status => group_by_status(items, folds),
    }
}

/// The row `<tab>`/`za` folds when the comments cursor sits at `at`: that
/// row's index when it is a header, otherwise the index of the header above
/// it. Mirrors `nav::foldable_at`, one level deep since a comment row nests
/// under exactly one header.
fn comment_foldable_at(rows: &[CommentPaneRow], at: usize) -> Option<usize> {
    let header = |row: &CommentPaneRow| matches!(row, CommentPaneRow::Header { .. });
    if header(rows.get(at)?) {
        return Some(at);
    }
    rows.get(..at)?.iter().rposition(header)
}

impl super::DiffView {
    pub fn comments_open(&self) -> bool {
        self.comments_open
    }

    pub fn comments_cursor(&self) -> usize {
        self.comments_cursor
    }
}

impl App {
    /// Where a comment's file sits in the diff, `usize::MAX` when the diff no
    /// longer carries it. Sorting the sidebar and asking whether a comment is
    /// orphaned are the same question.
    fn file_rank(&self, path: &str) -> usize {
        self.diff.as_ref().map_or(usize::MAX, |diff| {
            diff.model_for_rows(&self.review)
                .files
                .iter()
                .position(|file| file.path == path)
                .unwrap_or(usize::MAX)
        })
    }

    /// Comment ids of the active review, in the order the sidebar lists them:
    /// by file as the diff orders them, then by line.
    pub(crate) fn comment_order(&self) -> Vec<String> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let session = self.review.session_for(&diff.source);
        let mut ordered: Vec<&diffler_core::session::Comment> = session.comments.iter().collect();
        ordered.sort_by_key(|comment| {
            (
                self.file_rank(&comment.anchor.file),
                comment.anchor.line.unwrap_or(0),
            )
        });
        ordered.iter().map(|comment| comment.id.clone()).collect()
    }

    /// The pane's rows under its current grouping: headers and comments,
    /// folded groups' items left out. `comments_cursor` indexes into this,
    /// the way `tree_cursor` indexes into the file sidebar's own rows.
    pub(crate) fn comment_rows(&self) -> Vec<CommentPaneRow> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let session = self.review.session_for(&diff.source);
        let facts: Vec<CommentFacts> = self
            .comment_order()
            .into_iter()
            .filter_map(|id| {
                let comment = session.comment(&id)?;
                Some(CommentFacts {
                    orphan: self.file_rank(&comment.anchor.file) == usize::MAX,
                    id,
                    file: comment.anchor.file.clone(),
                    author: comment.author.clone(),
                    status: comment.status,
                })
            })
            .collect();
        group_comment_rows(&facts, diff.comment_grouping, &diff.comment_folds)
    }

    /// The sidebar is a pane of the diff screen, so it opens over a review
    /// that is already on screen.
    pub(crate) fn toggle_comments_sidebar(&mut self) {
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
        let count = self.comment_rows().len();
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
        let count = self.comment_rows().len();
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
        let count = self.comment_rows().len();
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if count == 0 {
            return;
        }
        diff.comments_cursor = index.min(count - 1);
        self.seat_cursor_on_selected_comment();
    }

    /// `[`/`]` in the comments pane: the previous/next group header, the way
    /// `]`/`[` step the file sidebar's own headers.
    pub(crate) fn comments_jump_header(&mut self, forward: bool) {
        let rows = self.comment_rows();
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let is_header = |row: &CommentPaneRow| matches!(row, CommentPaneRow::Header { .. });
        let Some(position) = crate::app::step_to(&rows, diff.comments_cursor, forward, is_header)
        else {
            return;
        };
        self.comments_to(position);
    }

    /// `tab`/`za` in the comments pane: fold the group the cursor sits in.
    pub(crate) fn comments_toggle_fold(&mut self) {
        let rows = self.comment_rows();
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let Some(target) = comment_foldable_at(&rows, diff.comments_cursor) else {
            self.info("nothing to fold here");
            return;
        };
        let Some(CommentPaneRow::Header { key, .. }) = rows.get(target) else {
            return;
        };
        let key = key.clone();
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if !diff.comment_folds.remove(&key) {
            diff.comment_folds.insert(key);
        }
        // the header that folded is the one to stand on, mirroring the file
        // sidebar's own za/<tab>; folding never removes a header's own row,
        // only what sits under it, so `target` always stays valid
        let rows = self.comment_rows();
        if let Some(diff) = self.diff.as_mut() {
            diff.comments_cursor = target.min(rows.len().saturating_sub(1));
        }
        self.seat_cursor_on_selected_comment();
    }

    /// `t` while the comments pane holds focus: cycle its grouping, keeping
    /// the selected comment selected where the new grouping still shows it.
    pub(crate) fn cycle_comment_grouping(&mut self) {
        let previous = self.selected_comment_id();
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.comment_grouping = diff.comment_grouping.cycle();
        let label = diff.comment_grouping.label();
        self.reseat_comments_cursor(previous);
        self.info(format!("comments: {label}"));
    }

    /// Land the comments cursor back on `previous` under the pane's current
    /// rows, or the top when it is hidden behind a group that grouping just
    /// folded (`status` starts with its resolved bucket closed).
    fn reseat_comments_cursor(&mut self, previous: Option<String>) {
        let rows = self.comment_rows();
        let target = previous
            .and_then(|id| {
                rows.iter()
                    .position(|row| matches!(row, CommentPaneRow::Item { id: at, .. } if *at == id))
            })
            .unwrap_or(0);
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.comments_cursor = target.min(rows.len().saturating_sub(1));
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

    /// The comment the sidebar has selected; `None` when it sits on a group
    /// header instead, the same as the file sidebar's cursor landing on a
    /// directory or section row.
    pub(crate) fn selected_comment_id(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        match self.comment_rows().get(diff.comments_cursor)? {
            CommentPaneRow::Item { id, .. } => Some(id.clone()),
            CommentPaneRow::Header { .. } => None,
        }
    }

    /// Whether `id` is anchored to a file outside this diff. The comment
    /// survives, since the file can come back on the next edit.
    pub(crate) fn comment_is_orphan(&self, id: &str) -> bool {
        let Some(diff) = self.diff.as_ref() else {
            return false;
        };
        self.review
            .session_for(&diff.source)
            .comment(id)
            .is_some_and(|comment| self.file_rank(&comment.anchor.file) == usize::MAX)
    }

    /// Delete the comment the sidebar has selected, by id: an orphan has no
    /// row for the cursor, and the cursor-driven delete would take whichever
    /// comment it was last left on.
    pub(crate) fn delete_selected_comment(&mut self) {
        let Some(id) = self.selected_comment_id() else {
            self.info("no comment selected");
            return;
        };
        self.confirm_delete_comment(&id);
    }

    /// Claim the comment the sidebar has selected, by id: an orphan has no
    /// row for the cursor, the same reason delete addresses it this way.
    pub(crate) fn claim_selected_comment(&mut self) {
        let Some(id) = self.selected_comment_id() else {
            self.info("no comment selected");
            return;
        };
        self.claim_comment(&id);
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
    /// lives in another one. This is what makes the pane's verbs apply. A
    /// header under the cursor selects no comment, so it seats nothing and
    /// leaves the diff cursor exactly where it was.
    pub(crate) fn seat_cursor_on_selected_comment(&mut self) {
        let Some(id) = self.selected_comment_id() else {
            return;
        };
        self.focus_comment(&id);
    }

    /// Enter the slide that holds `id`, so the walkthrough layout never shows
    /// a comment outside the slide on screen: the stop it is the primary of,
    /// else the stop whose region covers it, else a slide of its own. A
    /// comment reached this way always belongs to the open source's own
    /// walkthrough, since that source carries no other. Every route into a
    /// comment calls this; in any other layout it does nothing.
    pub(crate) fn enter_slide_for_comment(&mut self, id: &str) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        if diff.layout != crate::config::FileLayout::Walkthrough || self.comment_is_orphan(id) {
            return;
        }
        let session = self.review.session_for(&diff.source);
        let Some(walkthrough) = diff.active_walkthrough(session) else {
            return;
        };
        let holds = |stop: &String| {
            session
                .comments
                .iter()
                .position(|comment| comment.id == *stop)
                .is_some_and(|primary| {
                    crate::app::walkthrough::slide_comments(session, primary)
                        .into_iter()
                        .filter_map(|index| session.comments.get(index))
                        .any(|comment| comment.id == id)
                })
        };
        let slide = walkthrough
            .stops
            .iter()
            .position(|stop| stop == id)
            .or_else(|| walkthrough.stops.iter().position(holds));
        // the stop's own seating bands its region and opens its file, the
        // same arrival the sidebar gives
        if let Some(index) = slide {
            self.seat_stop(index);
            return;
        }
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.slide = Some(super::Slide::AdHoc(id.to_owned()));
        diff.referenced = None;
        diff.mark_rows_dirty();
    }

    /// Seat the diff cursor on the comment with `id`. Reports whether the
    /// comment's file is part of this diff at all.
    pub(crate) fn focus_comment(&mut self, id: &str) -> bool {
        self.enter_slide_for_comment(id);
        let Some(diff) = self.diff.as_ref() else {
            return false;
        };
        let session = self.review.session_for(&diff.source);
        let Some(comment_index) = session.comments.iter().position(|comment| comment.id == id)
        else {
            return false;
        };
        let Some(file) = session
            .comments
            .get(comment_index)
            .map(|c| c.anchor.file.clone())
        else {
            return false;
        };
        let model = diff.model_for_rows(&self.review);
        let Some(file_index) = model.files.iter().position(|entry| entry.path == file) else {
            self.info("comment file is not in this diff");
            return false;
        };
        let span = session
            .comments
            .get(comment_index)
            .and_then(|comment| comment.anchor.span());
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return false;
        };
        let seated = diff
            .seat_on(review, file_index, |row| {
                matches!(row, DiffRow::Comment { comment, line: 0, .. } if *comment == comment_index)
            })
            .is_some();
        // a jump lands on the card, so the lines it speaks about are banded to
        // say which of the code around it the reader was sent to
        diff.referenced =
            span.and_then(|(line, end)| diff.span_rows(review, file_index, line, end));
        seated
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

    fn selected_comment_is_orphan(app: &App) -> bool {
        app.selected_comment_id()
            .is_some_and(|id| app.comment_is_orphan(&id))
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

    /// A file can leave the diff under an open review (the agent reverts it),
    /// stranding its comments with no row in the pane.
    #[test]
    fn deleting_an_orphaned_comment_takes_that_one_and_no_other() {
        let (_fixture, mut app) = app_with_comments();
        app.review
            .session
            .add_comment(anchor("gone.rs", 1), "reviewer", "orphan");
        app.handle(key('C'));
        app.handle(key('G'));
        assert!(selected_comment_is_orphan(&app), "the orphan sorts last");

        app.handle(key('d'));
        app.handle(key('y'));

        let left: Vec<&str> = app
            .review
            .session
            .comments
            .iter()
            .map(|comment| comment.body.as_str())
            .collect();
        assert_eq!(left, vec!["why 42?", "stale note"], "only the orphan went");
    }

    /// A verb that reads the diff cursor or the selected file finds neither on
    /// an orphan: `v` would mark a file the reader never opened as viewed, and
    /// `y`/`e` would take a third, unrelated file.
    #[test]
    fn an_orphaned_comment_declines_the_verbs_that_need_a_row() {
        for verb in ['r', 'R', 'c', 'V', 'v', 'y', 'e'] {
            let (_fixture, mut app) = app_with_comments();
            app.review
                .session
                .add_comment(anchor("gone.rs", 1), "reviewer", "orphan");
            app.handle(key('C'));
            app.handle(key('G'));
            assert!(selected_comment_is_orphan(&app));

            app.handle(key(verb));

            assert!(!app.composer_open(), "{verb}: no composer opened");
            assert!(
                app.review.session.viewed.is_empty(),
                "{verb}: nothing was marked viewed"
            );
            assert!(
                app.pending_clipboard.is_none(),
                "{verb}: nothing was yanked"
            );
            assert!(app.pending_editor.is_none(), "{verb}: no editor was opened");
            assert!(
                app.message
                    .as_ref()
                    .is_some_and(|m| m.text.contains("not in this diff")),
                "{verb}: {:?}",
                app.message
            );
        }
    }

    /// The review-wide verbs address the whole review, so an orphan sitting
    /// under the cursor is no reason to refuse them.
    #[test]
    fn an_orphaned_selection_still_allows_the_verbs_that_need_no_row() {
        let (_fixture, mut app) = app_with_comments();
        app.review
            .session
            .add_comment(anchor("gone.rs", 1), "reviewer", "orphan");
        app.handle(key('C'));
        app.handle(key('G'));
        assert!(selected_comment_is_orphan(&app));

        app.handle(key('D'));

        assert!(
            matches!(app.modal, Some(crate::app::Modal::Confirm { .. })),
            "D starts the review wipe: {:?}",
            app.message
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
    fn c_on_the_status_screen_opens_no_review() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('C'));
        assert!(
            app.diff.is_none(),
            "the comments sidebar is a pane of a review, not a way into one"
        );
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

    /// Three comments across two files, two authors, and one resolved, for
    /// exercising the pane's groupings: file, author, and status all differ.
    /// Returns the resolved comment's id alongside the fixture.
    fn app_with_grouped_comments() -> (Fixture, App, String) {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.review
            .session
            .add_comment(anchor("src/lib.rs", 2), "reviewer", "why 42?");
        app.review
            .session
            .add_comment(anchor("todo.md", 1), "alice", "needs a date");
        let resolved = app
            .review
            .session
            .add_comment(anchor("todo.md", 3), "alice", "already fixed")
            .id
            .clone();
        app.review.session.resolve(&resolved);
        app.open_working_tree_diff(None);
        (fixture, app, resolved)
    }

    fn header_rows(app: &App) -> Vec<(String, usize, bool)> {
        app.comment_rows()
            .into_iter()
            .filter_map(|row| match row {
                CommentPaneRow::Header {
                    label,
                    count,
                    folded,
                    ..
                } => Some((label, count, folded)),
                CommentPaneRow::Item { .. } => None,
            })
            .collect()
    }

    #[test]
    fn the_comments_pane_opens_flat_with_no_headers() {
        let (_fixture, mut app, _resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        assert_eq!(
            app.diff.as_ref().expect("diff").comment_grouping,
            CommentGrouping::Flat
        );
        assert!(
            header_rows(&app).is_empty(),
            "a flat list has no group headers"
        );
    }

    #[test]
    fn t_cycles_the_comments_pane_through_its_four_groupings_and_back() {
        let (_fixture, mut app, _resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        let grouping = |app: &App| app.diff.as_ref().expect("diff").comment_grouping;
        for expected in [
            CommentGrouping::File,
            CommentGrouping::Author,
            CommentGrouping::Status,
            CommentGrouping::Flat,
        ] {
            app.dispatch(Action::CycleSidebarMode);
            assert_eq!(grouping(&app), expected);
        }
    }

    #[test]
    fn t_elsewhere_still_cycles_the_file_sidebars_own_layout() {
        let (_fixture, mut app, _resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        app.handle(key('h')); // Comments -> Diff
        app.handle(key('h')); // Diff -> the file list
        assert_eq!(app.diff.as_ref().expect("diff").focus, Pane::List);

        app.dispatch(Action::CycleSidebarMode);

        assert_eq!(
            app.diff.as_ref().expect("diff").layout,
            crate::config::FileLayout::Review,
            "t in the file sidebar keeps cycling tree/review/kinds"
        );
        assert_eq!(
            app.diff.as_ref().expect("diff").comment_grouping,
            CommentGrouping::Flat,
            "the comments pane's own grouping never moved"
        );
    }

    #[test]
    fn grouping_by_file_headers_each_file_once_in_diff_order_with_its_count() {
        let (_fixture, mut app, _resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        app.dispatch(Action::CycleSidebarMode); // -> File

        let model_order: Vec<String> = app
            .diff
            .as_ref()
            .expect("diff")
            .model(&app.review)
            .files
            .iter()
            .map(|f| f.path.clone())
            .filter(|p| p == "src/lib.rs" || p == "todo.md")
            .collect();
        let counts: HashMap<&str, usize> = [("src/lib.rs", 1), ("todo.md", 2)].into();
        let expected: Vec<(String, usize, bool)> = model_order
            .iter()
            .map(|path| (path.clone(), counts[path.as_str()], false))
            .collect();
        assert_eq!(
            header_rows(&app),
            expected,
            "one header per file, in the order the diff lists them"
        );
    }

    #[test]
    fn grouping_by_author_headers_each_author_once_with_its_count() {
        let (_fixture, mut app, _resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        app.dispatch(Action::CycleSidebarMode); // File
        app.dispatch(Action::CycleSidebarMode); // Author

        assert_eq!(
            header_rows(&app),
            vec![
                ("reviewer".to_owned(), 1, false),
                ("alice".to_owned(), 2, false),
            ],
            "reviewer's own comment sorts first, then alice's two"
        );
    }

    #[test]
    fn grouping_by_status_orders_open_replied_resolved_and_starts_resolved_folded() {
        let (_fixture, mut app, resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        app.dispatch(Action::CycleSidebarMode); // File
        app.dispatch(Action::CycleSidebarMode); // Author
        app.dispatch(Action::CycleSidebarMode); // Status

        assert_eq!(
            header_rows(&app),
            vec![
                ("Open".to_owned(), 2, false),
                ("Resolved".to_owned(), 1, true),
            ],
            "open leads, resolved trails and starts folded"
        );
        assert!(
            !app.comment_rows()
                .iter()
                .any(|row| matches!(row, CommentPaneRow::Item { id, .. } if *id == resolved)),
            "the resolved comment's own row is hidden behind its folded header"
        );
    }

    #[test]
    fn tab_folds_the_header_the_cursor_sits_under_and_lands_on_it() {
        let (_fixture, mut app, _resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        app.dispatch(Action::CycleSidebarMode); // -> File
        let before = app.comment_rows().len();
        // stand on the single-comment file's own item row
        let item_row = app
            .comment_rows()
            .iter()
            .position(|row| matches!(row, CommentPaneRow::Item { .. }))
            .expect("a comment row exists");
        app.diff.as_mut().expect("diff").comments_cursor = item_row;

        app.dispatch(Action::ToggleFold);

        let rows = app.comment_rows();
        assert!(rows.len() < before, "folding hides the header's items");
        let cursor = app.diff.as_ref().expect("diff").comments_cursor();
        assert!(
            matches!(
                rows.get(cursor),
                Some(CommentPaneRow::Header { folded: true, .. })
            ),
            "the header the item sat under is now folded, and the cursor stands on it: {rows:?}"
        );
    }

    #[test]
    fn bracket_keys_step_group_headers_in_the_comments_pane() {
        let (_fixture, mut app, _resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        app.dispatch(Action::CycleSidebarMode); // -> File
        app.diff.as_mut().expect("diff").comments_cursor = 0;

        app.dispatch(Action::NextHunk);
        let rows = app.comment_rows();
        let cursor = app.diff.as_ref().expect("diff").comments_cursor();
        assert!(
            matches!(rows.get(cursor), Some(CommentPaneRow::Header { .. })),
            "] lands on the next header"
        );
        assert!(cursor > 0, "and it is not the one the cursor started on");

        app.dispatch(Action::PrevHunk);
        assert_eq!(
            app.diff.as_ref().expect("diff").comments_cursor(),
            0,
            "[ steps back to the first header"
        );
    }

    #[test]
    fn a_header_under_the_cursor_selects_no_comment_and_declines_its_verbs() {
        let (_fixture, mut app, _resolved) = app_with_grouped_comments();
        app.handle(key('C'));
        app.dispatch(Action::CycleSidebarMode); // -> File
        app.diff.as_mut().expect("diff").comments_cursor = 0;
        assert!(
            matches!(
                app.comment_rows().first(),
                Some(CommentPaneRow::Header { .. })
            ),
            "row 0 is a header under the file grouping"
        );

        assert_eq!(app.selected_comment_id(), None);

        app.handle(key('c'));

        assert!(!app.composer_open(), "a header opens no composer");
        assert_eq!(
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("no comment selected")
        );
    }
}
