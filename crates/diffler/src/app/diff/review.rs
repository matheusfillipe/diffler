//! What the human does to a diff once they are reading it: leaving comments,
//! replying, resolving, marking files viewed, and exporting the feedback.

use diffler_core::session::{Anchor, Comment, CommentStatus};

use diffler_core::feedback::{self, FeedbackOptions};
use diffler_core::model::{DiffModel, FileDiff};
use diffler_core::source::ReviewSource;

use super::{ComposerKind, DiffRow, Pane, Slide, sidebar_rows};
use crate::app::rowsel::RowSelect;
use crate::app::{App, Modal};
use crate::config::FileLayout;
use crate::tree::TreeNode;

impl App {
    /// Anchor for a new comment at the cursor (or the visual selection).
    fn comment_anchor(&self) -> Option<Anchor> {
        let diff = self.diff.as_ref()?;
        let model_cow = diff.model_for_rows(&self.review);
        let model: &DiffModel = &model_cow;
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
        let (file_idx, _, line) = line_at(diff.rows.get(anchor_row)?)?;
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
            // the display target is the range end, so that is the line
            // whose drift marks the comment outdated
            line_text: Some(last_text),
        })
    }

    pub(super) fn comment_at_cursor(&mut self) {
        // `c` over an existing comment edits it; otherwise it starts a new one
        if let Some(comment) = self.comment_at_cursor_row() {
            let comment_id = comment.id.clone();
            let body = comment.body.clone();
            self.open_composer(ComposerKind::Edit { comment_id }, body);
            return;
        }
        let Some(anchor) = self.comment_anchor() else {
            self.info("move to a diff line to comment");
            return;
        };
        self.open_composer(ComposerKind::New { anchor }, String::new());
    }

    /// `c` in the file sidebar: a whole-file comment (a line-less anchor) on the
    /// selected file, rendered above that file's diff.
    pub(super) fn comment_on_selected_file(&mut self) {
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
            line_text: None,
        };
        self.open_composer(ComposerKind::New { anchor }, String::new());
    }

    fn comment_at_cursor_row(&self) -> Option<&Comment> {
        let diff = self.diff.as_ref()?;
        let DiffRow::Comment { comment, .. } = diff.rows.get(diff.cursor)? else {
            return None;
        };
        self.review.session_for(&diff.source).comments.get(*comment)
    }

    pub(super) fn delete_comment_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to delete it");
            return;
        };
        let id = comment.id.clone();
        self.confirm_delete_comment(&id);
    }

    /// Ask before deleting the comment with `id`, whatever pointed at it.
    pub(crate) fn confirm_delete_comment(&mut self, id: &str) {
        let source = self.active_review_source();
        let Some(author) = self
            .review
            .session_for(&source)
            .comment(id)
            .map(|comment| comment.author.clone())
        else {
            return;
        };
        self.modal = Some(Modal::Confirm {
            message: format!("Delete {author}'s comment?"),
            on_confirm: crate::app::PendingOp::DeleteComment(id.to_owned()),
        });
    }

    pub(super) fn claim_comment_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to claim it");
            return;
        };
        let id = comment.id.clone();
        self.claim_comment(&id);
    }

    /// Flip one comment between the agent's authorship and the human's: an
    /// agent comment becomes the human's own, so it goes out with the next
    /// submitted review, and a second press hands it back. Any other author
    /// (a synced forge comment, say) is left alone. A walkthrough's own
    /// comments are never posted anywhere, and claiming one would make it
    /// invisible to the revision that is meant to prune it, so a walkthrough
    /// source refuses the whole verb.
    pub(crate) fn claim_comment(&mut self, id: &str) {
        let source = self.active_review_source();
        if matches!(source, ReviewSource::Walkthrough { .. }) {
            self.info("a walkthrough's own comment isn't claimable");
            return;
        }
        let author = self.author.clone();
        let session = self.review.session_for_mut(&source);
        let Some(comment) = session.comments.iter_mut().find(|c| c.id == id) else {
            self.info("comment is gone");
            return;
        };
        let claimed = if comment.author == crate::mcp::AGENT_AUTHOR {
            comment.author = author;
            Some(true)
        } else if comment.author == author {
            crate::mcp::AGENT_AUTHOR.clone_into(&mut comment.author);
            Some(false)
        } else {
            None
        };
        match claimed {
            Some(true) => {
                self.after_session_change();
                self.info("claimed the comment as you");
            }
            Some(false) => {
                self.after_session_change();
                self.info("handed the comment back to the agent");
            }
            None => self.info("not an agent comment to claim"),
        }
    }

    /// `A`: ask before claiming every agent comment of the active review as
    /// the human's own, the way it goes out with a submitted review. A
    /// walkthrough source refuses the same way `claim_comment` does.
    pub(super) fn claim_all_comments_start(&mut self) {
        let source = self.active_review_source();
        if matches!(source, ReviewSource::Walkthrough { .. }) {
            self.info("a walkthrough's own comments aren't claimable");
            return;
        }
        let count = self
            .review
            .session_for(&source)
            .comments
            .iter()
            .filter(|c| c.author == crate::mcp::AGENT_AUTHOR)
            .count();
        if count == 0 {
            self.info("no agent comments to claim");
            return;
        }
        self.modal = Some(Modal::Confirm {
            message: format!("Claim all {count} agent comments as yours?"),
            on_confirm: crate::app::PendingOp::ClaimAllComments,
        });
    }

    /// Forge comments survive the wipe, so the question counts the local ones.
    pub(super) fn delete_all_comments_start(&mut self) {
        let source = self.active_review_source();
        let local = self
            .review
            .session_for(&source)
            .comments
            .iter()
            .filter(|comment| comment.remote_id.is_none())
            .count();
        if local == 0 {
            self.info("no comments to delete");
            return;
        }
        self.modal = Some(Modal::Confirm {
            message: format!("Delete all {local} comments of this review?"),
            on_confirm: crate::app::PendingOp::DeleteAllComments,
        });
    }

    pub(super) fn reply_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to reply");
            return;
        };
        let comment_id = comment.id.clone();
        self.open_composer(ComposerKind::Reply { comment_id }, String::new());
    }

    /// `R`: toggle the comment's resolution. In a PR review the flip is
    /// optimistic and syncs to the forge thread; elsewhere it stays local.
    pub(super) fn resolve_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to resolve");
            return;
        };
        let id = comment.id.clone();
        let resolving = comment.status != CommentStatus::Resolved;
        let forge = comment.remote_id.is_some();
        let source = self.active_review_source();
        // a forge thread must be queueable before the local flip, or the
        // status would lie until the next sync reverts it
        if forge
            && let ReviewSource::Pr { number } = source
            && !self.queue_pr_resolve(number, &id, resolving)
        {
            return;
        }
        let session = self.review.session_for_mut(&source);
        if resolving {
            session.resolve(&id);
        } else if let Some(comment) = session.comments.iter_mut().find(|c| c.id == id) {
            comment.status = CommentStatus::Open;
        }
        self.after_session_change();
        self.info(if resolving {
            "comment resolved"
        } else {
            "comment reopened"
        });
    }

    pub(super) fn diff_toggle_viewed(&mut self) {
        if self
            .diff
            .as_ref()
            .is_some_and(|diff| diff.layout == FileLayout::Walkthrough)
        {
            return self.walkthrough_toggle_seen();
        }
        if self.diff_toggle_group_viewed() {
            return;
        }
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
        let viewed = self.review.session_for(&source).is_viewed(&path, &hash);
        // marking walks the review file by file, so the cursor lands on the row
        // listed under this one. Read before the toggle: marking sorts the file
        // up into its group's viewed run, moving the rows underneath it
        let next = if viewed {
            None
        } else {
            self.sidebar_file_below()
        };
        let anchor_row = self.diff.as_ref().map_or(0, |diff| diff.tree_cursor);
        let session = self.review.session_for_mut(&source);
        if viewed {
            session.unmark_viewed(&path);
        } else {
            session.mark_viewed(&path, &hash);
        }
        if let Err(err) = self.review.save_for(&source) {
            self.error(err.to_string());
        }
        match next {
            Some(index) => self.diff_select_file_index(index),
            // nothing below to walk to. The file just sorted into its group's
            // viewed run, so following it would throw the reader to wherever it
            // landed; hold the row they were reading at instead
            None if !viewed => {
                self.diff_tree_to(anchor_row);
                let (total, seen) = self.viewed_counts();
                if total > seen {
                    self.info(format!("end of the list, {} still unviewed", total - seen));
                }
            }
            None => {}
        }
        // the toggle can reshuffle the review layout's buckets without the
        // advance re-seating anything (unmark, or nothing left to advance to)
        let review = &self.review;
        if let Some(diff) = self.diff.as_mut() {
            let rows = sidebar_rows(diff, review);
            diff.reseat_tree_cursor(&rows);
        }
    }

    /// `v` on any sidebar header marks everything it holds, so a whole subtree
    /// or a whole kind clears in one keystroke. Reports whether it handled the
    /// key. Already-viewed throughout means the press unmarks instead, matching
    /// how a single file toggles.
    fn diff_toggle_group_viewed(&mut self) -> bool {
        let review = &self.review;
        let Some(diff) = self.diff.as_ref() else {
            return false;
        };
        if diff.focus != Pane::List {
            return false;
        }
        let rows = sidebar_rows(diff, review);
        let model = diff.model(review);
        let session = review.session_for(&diff.source);
        let files: Vec<(String, String)> = match rows.get(diff.tree_cursor).map(|row| &row.node) {
            Some(TreeNode::Dir { path, .. }) => {
                let prefix = format!("{path}/");
                model
                    .files
                    .iter()
                    .filter(|file| file.path.starts_with(&prefix))
                    .map(|file| (file.path.clone(), file.content_hash()))
                    .collect()
            }
            // read from the bucket, not the rows: a folded header lists none of
            // its files and still stands for all of them
            Some(TreeNode::Section { bucket, .. }) => diff
                .bucket_files(model, session, *bucket)
                .into_iter()
                .filter_map(|index| model.files.get(index))
                .map(|file| (file.path.clone(), file.content_hash()))
                .collect(),
            Some(TreeNode::File { .. } | TreeNode::Stop { .. } | TreeNode::WalkthroughSummary)
            | None => return false,
        };
        if files.is_empty() {
            return false;
        }
        let source = self.active_review_source();
        let session = self.review.session_for(&source);
        let all_viewed = files
            .iter()
            .all(|(path, hash)| session.is_viewed(path, hash));
        let session = self.review.session_for_mut(&source);
        for (path, hash) in &files {
            if all_viewed {
                session.unmark_viewed(path);
            } else {
                session.mark_viewed(path, hash);
            }
        }
        if let Err(err) = self.review.save_for(&source) {
            self.error(err.to_string());
        }
        let count = files.len();
        self.info(if all_viewed {
            format!("{count} files back to unviewed")
        } else {
            format!("{count} files marked viewed")
        });
        // the buckets reshuffle under the cursor in the review layout
        let review = &self.review;
        if let Some(diff) = self.diff.as_mut() {
            let rows = sidebar_rows(diff, review);
            diff.reseat_tree_cursor(&rows);
        }
        true
    }

    pub(super) fn diff_unview_all(&mut self) {
        let source = self.active_review_source();
        let session = self.review.session_for_mut(&source);
        if session.viewed.is_empty() {
            self.info("no files marked viewed");
            return;
        }
        session.clear_viewed();
        if let Err(err) = self.review.save_for(&source) {
            self.error(err.to_string());
            return;
        }
        // every file returns to the to-review bucket, so re-seat the sidebar
        let review = &self.review;
        if let Some(diff) = self.diff.as_mut() {
            let rows = sidebar_rows(diff, review);
            diff.reseat_tree_cursor(&rows);
        }
        self.info("cleared all viewed marks");
    }

    /// The stop index of the slide currently open, when it is a stop's own
    /// (not the all-slides view, an ad hoc comment, or nothing seated yet).
    fn current_stop_index(&self) -> Option<usize> {
        match self.diff.as_ref()?.slide {
            Some(Slide::Stop(index)) => Some(index),
            _ => None,
        }
    }

    /// `m` in the walkthrough layout: toggle the current slide's seen mark,
    /// then advance to the next slide the way `m` on a file advances to the
    /// row below it. Unmarking holds still, matching the file behaviour.
    pub(super) fn walkthrough_toggle_seen(&mut self) {
        let Some(index) = self.current_stop_index() else {
            return;
        };
        let Some(id) = self
            .active_walkthrough()
            .and_then(|w| w.stops.get(index).cloned())
        else {
            return;
        };
        let source = self.active_review_source();
        let seen = self.review.session_for(&source).is_stop_seen(&id);
        let session = self.review.session_for_mut(&source);
        if seen {
            session.unmark_stop_seen(&id);
        } else {
            session.mark_stop_seen(&id);
        }
        let _ = self.persist_review_change(&source);
        if seen {
            return;
        }
        let total = self.active_walkthrough().map_or(0, |w| w.stops.len());
        if index + 1 < total {
            self.seat_stop(index + 1);
            return;
        }
        let source = self.active_review_source();
        let session = self.review.session_for(&source);
        let seen_count = self.active_walkthrough().map_or(0, |w| {
            w.stops.iter().filter(|id| session.is_stop_seen(id)).count()
        });
        if total > seen_count {
            self.info(format!(
                "end of the walkthrough, {} still unseen",
                total - seen_count
            ));
        }
    }

    /// `u` in the walkthrough layout: jump to the next slide not yet marked
    /// seen, wrapping past the end.
    pub(super) fn walkthrough_jump_unseen(&mut self) {
        let Some(walkthrough) = self.active_walkthrough().cloned() else {
            return;
        };
        let source = self.active_review_source();
        let total = walkthrough.stops.len();
        if total == 0 {
            return;
        }
        let session = self.review.session_for(&source);
        let start = self.current_stop_index().map_or(0, |index| index + 1);
        let next = (0..total).map(|step| (start + step) % total).find(|index| {
            walkthrough
                .stops
                .get(*index)
                .is_some_and(|id| !session.is_stop_seen(id))
        });
        match next {
            Some(index) => self.seat_stop(index),
            None => self.info("every slide is seen"),
        }
    }

    /// The file the sidebar lists under the selected one, skipping headers and
    /// whatever a folded group hides. `None` at the end of the list, where the
    /// selection stays put.
    fn sidebar_file_below(&self) -> Option<usize> {
        let diff = self.diff.as_ref()?;
        let rows = sidebar_rows(diff, &self.review);
        let at = super::tree_position_of_file(&rows, diff.selected)?;
        let below = super::step_file_row(&rows, at, true)?;
        super::row_file_index(rows.get(below)?)
    }

    /// Land the pane on the model file at `index` and seat the tree cursor on
    /// its row. Used where a file is chosen by model index (the viewed walk,
    /// scoped open) rather than by tree position.
    pub(super) fn diff_select_file_index(&mut self, index: usize) {
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

    pub(super) fn copy_feedback(&mut self, file_only: bool) {
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
        let title = format!("Review feedback: {repo} @ {branch} ({count} {noun})");
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

    /// `y`: with a visual selection, yank whatever rows it covers through the
    /// shared row-text path every other screen uses; with none, keep the
    /// review's own meaning for the key, exporting this file's comments as
    /// markdown, since a reader relying on that fallback sees nothing change.
    pub(super) fn copy_file_or_selection(&mut self) {
        if self
            .diff
            .as_ref()
            .is_some_and(|diff| diff.selection().is_some())
        {
            self.yank_rows("yanked selection");
        } else {
            self.copy_feedback(true);
        }
    }
}
