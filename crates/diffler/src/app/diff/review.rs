//! What the human does to a diff once they are reading it: leaving comments,
//! replying, resolving, marking files viewed, and exporting the feedback.

use diffler_core::session::{Anchor, Comment, CommentStatus};

use diffler_core::feedback::{self, FeedbackOptions};
use diffler_core::model::{FileDiff, LineKind};
use diffler_core::source::ReviewSource;

use super::{ComposerKind, DiffRow, next_unviewed_index, sidebar_rows};
use crate::app::{App, Modal};

impl App {
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
        let (id, author) = (comment.id.clone(), comment.author.clone());
        self.modal = Some(Modal::Confirm {
            message: format!("Delete {author}'s comment?"),
            on_confirm: crate::app::PendingOp::DeleteComment(id),
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
        // the toggle can reshuffle the review layout's buckets without the
        // advance re-seating anything (unmark, or nothing left to advance to)
        let review = &self.review;
        if let Some(diff) = self.diff.as_mut() {
            let rows = sidebar_rows(diff, review);
            diff.reseat_tree_cursor(&rows);
        }
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

    /// Move the sidebar selection to the next not-viewed file below it, if
    /// any; otherwise stay put.
    fn diff_advance_to_unviewed(&mut self) {
        let next = self
            .diff
            .as_ref()
            .and_then(|diff| next_unviewed_index(diff, &self.review, false));
        if let Some(index) = next {
            self.diff_select_file_index(index);
        }
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

    pub(super) fn copy_file_or_selection(&mut self) {
        if !self.copy_selection() {
            self.copy_feedback(true);
        }
    }
}
