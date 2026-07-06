//! Modal and input handling, including the branch prompts.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use std::path::Path;

use super::{App, BranchAction, Flow, InputOp, Modal, PendingOp, byte_index};

impl App {
    pub(super) fn handle_modal_key(&mut self, key: &KeyEvent) -> Flow {
        match &self.modal {
            Some(Modal::Confirm { .. }) => match key.code {
                KeyCode::Char('y') => self.confirm_modal(),
                KeyCode::Char('n') | KeyCode::Esc => self.modal = None,
                _ => {}
            },
            Some(Modal::Input { .. }) => self.handle_input_key(key),
            Some(Modal::BranchList { .. }) => self.handle_branch_list_key(key),
            Some(Modal::Comments { .. }) => self.handle_comments_key(key),
            Some(Modal::Help) => match key.code {
                KeyCode::Esc | KeyCode::Char('q' | '?') => self.modal = None,
                _ => {}
            },
            None => {}
        }
        Flow::Continue
    }

    pub(super) fn confirm_modal(&mut self) {
        let Some(Modal::Confirm { on_confirm, .. }) = self.modal.take() else {
            return;
        };
        match on_confirm {
            PendingOp::Discard { path } => {
                self.vcs_op(move |vcs| vcs.discard(Path::new(&path)));
            }
            PendingOp::DeleteBranch(name) => {
                self.message = None;
                self.vcs_op(|vcs| vcs.delete_branch(&name));
                if self.message.is_none() {
                    self.info(format!("deleted branch {name}"));
                }
            }
            PendingOp::DeleteAllComments => self.delete_all_comments(),
        }
    }

    pub(super) fn handle_input_key(&mut self, key: &KeyEvent) {
        // Alt-Enter inserts a newline; Ctrl-J is the fallback for terminals
        // that swallow the alt modifier
        let newline = (key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::ALT))
            || (key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL));
        match key.code {
            _ if newline => {
                let Some(Modal::Input { buffer, cursor, .. }) = self.modal.as_mut() else {
                    return;
                };
                buffer.insert(byte_index(buffer, *cursor), '\n');
                *cursor += 1;
            }
            KeyCode::Esc => self.modal = None,
            KeyCode::Enter => self.submit_input(),
            code => {
                let Some(Modal::Input { buffer, cursor, .. }) = self.modal.as_mut() else {
                    return;
                };
                match code {
                    KeyCode::Char(c) => {
                        buffer.insert(byte_index(buffer, *cursor), c);
                        *cursor += 1;
                    }
                    KeyCode::Backspace => {
                        if *cursor > 0 {
                            *cursor -= 1;
                            buffer.remove(byte_index(buffer, *cursor));
                        }
                    }
                    KeyCode::Left => *cursor = cursor.saturating_sub(1),
                    KeyCode::Right => *cursor = (*cursor + 1).min(buffer.chars().count()),
                    KeyCode::Home => *cursor = 0,
                    KeyCode::End => *cursor = buffer.chars().count(),
                    _ => {}
                }
            }
        }
    }

    /// An empty buffer submits as a cancel — comments and replies must say
    /// Open the text-input modal with the cursor at the end of `buffer` (so a
    /// prefilled edit lands ready to append). An empty buffer starts at column 0.
    pub(crate) fn open_input(&mut self, title: String, buffer: String, on_submit: InputOp) {
        self.modal = Some(Modal::Input {
            cursor: buffer.chars().count(),
            buffer,
            title,
            on_submit,
        });
    }

    /// something to be worth persisting.
    pub(super) fn submit_input(&mut self) {
        let Some(Modal::Input {
            buffer, on_submit, ..
        }) = self.modal.take()
        else {
            return;
        };
        let body = buffer.trim();
        if body.is_empty() {
            return;
        }
        let source = self.active_review_source();
        match on_submit {
            InputOp::Comment { anchor } => {
                self.review
                    .session_for_mut(&source)
                    .add_comment(&self.author, anchor, body);
                if let Some(diff) = self.diff.as_mut() {
                    diff.visual_anchor = None;
                }
                self.after_session_change();
            }
            InputOp::Reply { comment_id } => {
                if self
                    .review
                    .session_for_mut(&source)
                    .reply(&comment_id, &self.author, body)
                {
                    self.after_session_change();
                } else {
                    self.error("comment is gone; reply dropped");
                }
            }
            InputOp::EditComment { comment_id } => {
                if self
                    .review
                    .session_for_mut(&source)
                    .edit_comment(&comment_id, body)
                {
                    self.after_session_change();
                } else {
                    self.error("comment is gone; edit dropped");
                }
            }
            InputOp::CreateBranch { checkout } => {
                let name = body.to_owned();
                self.message = None;
                self.vcs_op(|vcs| vcs.create_branch(&name, checkout));
                if self.message.is_none() {
                    if checkout {
                        self.info(format!("switched to new branch {name}"));
                    } else {
                        self.info(format!("created branch {name}"));
                    }
                }
            }
        }
    }

    /// Current feedback epoch (see [`App::feedback_tx`]).
    pub(super) fn branch_name_input(&mut self, checkout: bool) {
        let title = if checkout {
            "New branch (checkout)"
        } else {
            "New branch"
        };
        self.open_input(
            title.to_owned(),
            String::new(),
            InputOp::CreateBranch { checkout },
        );
    }

    pub(super) fn open_branch_list(&mut self, action: BranchAction) {
        match self.review.vcs.branches() {
            Ok(branches) if branches.is_empty() => {
                self.modal = None;
                self.info("no branches");
            }
            Ok(branches) => {
                self.modal = Some(Modal::BranchList {
                    branches,
                    cursor: 0,
                    action,
                });
            }
            Err(err) => {
                self.modal = None;
                self.error(err.to_string());
            }
        }
    }

    /// The comments overview: every comment of the active review, ordered by
    /// file then line, Enter jumping to the comment in the diff pane.
    pub(super) fn open_comments_overview(&mut self) {
        let source = self.active_review_source();
        let model = self.source_model(&source);
        let session = self.review.session_for(&source);
        let file_order = |path: &str| {
            model
                .files
                .iter()
                .position(|f| f.path == path)
                .unwrap_or(usize::MAX)
        };
        let mut entries: Vec<super::CommentJump> = session
            .comments
            .iter()
            .map(|comment| {
                let line = comment
                    .anchor
                    .line
                    .map_or(String::new(), |l| format!(":{l}"));
                let status = match comment.status {
                    diffler_core::session::CommentStatus::Open => "open",
                    diffler_core::session::CommentStatus::Replied => "replied",
                    diffler_core::session::CommentStatus::Resolved => "resolved",
                };
                let snippet = comment.body.lines().next().unwrap_or("").to_owned();
                let replies = if comment.replies.is_empty() {
                    String::new()
                } else {
                    format!(" +{}", comment.replies.len())
                };
                super::CommentJump {
                    file: comment.anchor.file.clone(),
                    comment_id: comment.id.clone(),
                    label: format!(
                        "{}{line} · {} · {status}{replies} · {snippet}",
                        comment.anchor.file, comment.author
                    ),
                }
            })
            .collect();
        entries.sort_by_key(|e| file_order(&e.file));
        if entries.is_empty() {
            self.info("no comments in this review yet");
            return;
        }
        self.modal = Some(Modal::Comments { entries, cursor: 0 });
    }

    pub(super) fn handle_comments_key(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
            KeyCode::Char('d') => self.delete_selected_overview_comment(),
            KeyCode::Char('D') => {
                let entries = match &self.modal {
                    Some(Modal::Comments { entries, .. }) => entries.len(),
                    _ => 0,
                };
                self.modal = Some(Modal::Confirm {
                    message: format!("Delete all {entries} comments of this review?"),
                    on_confirm: super::PendingOp::DeleteAllComments,
                });
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(Modal::Comments { entries, cursor }) = self.modal.as_mut() {
                    *cursor = (*cursor + 1).min(entries.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(Modal::Comments { cursor, .. }) = self.modal.as_mut() {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Char('g') => {
                if let Some(Modal::Comments { cursor, .. }) = self.modal.as_mut() {
                    *cursor = 0;
                }
            }
            KeyCode::Char('G') => {
                if let Some(Modal::Comments { entries, cursor }) = self.modal.as_mut() {
                    *cursor = entries.len().saturating_sub(1);
                }
            }
            KeyCode::Enter => self.jump_to_selected_comment(),
            _ => {}
        }
    }

    /// Delete the highlighted overview entry and rebuild the list in place.
    fn delete_selected_overview_comment(&mut self) {
        let (entry, keep_cursor) = match &self.modal {
            Some(Modal::Comments { entries, cursor }) => match entries.get(*cursor).cloned() {
                Some(entry) => (entry, cursor.saturating_sub(1)),
                None => return,
            },
            _ => return,
        };
        if self.delete_comment_by_id(&entry.comment_id) {
            self.open_comments_overview();
            if let Some(Modal::Comments { entries, cursor }) = self.modal.as_mut() {
                *cursor = keep_cursor.min(entries.len().saturating_sub(1));
            }
        }
    }

    /// Delete one comment outright. Forge-owned comments decline — the next
    /// sync would just re-import them.
    pub(super) fn delete_comment_by_id(&mut self, id: &str) -> bool {
        let source = self.active_review_source();
        let session = self.review.session_for_mut(&source);
        let forge_owned = session
            .comments
            .iter()
            .any(|c| c.id == id && c.remote_id.is_some());
        if forge_owned {
            self.info("forge comment — resolve or delete it on the PR");
            return false;
        }
        if !session.delete_comment(id) {
            return false;
        }
        self.after_session_change();
        true
    }

    /// Start fresh: drop every local comment of the active review (forge-owned
    /// ones stay; the forge is their home).
    pub(super) fn delete_all_comments(&mut self) {
        let source = self.active_review_source();
        let session = self.review.session_for_mut(&source);
        let before = session.comments.len();
        session.comments.retain(|c| c.remote_id.is_some());
        let removed = before - session.comments.len();
        let kept = session.comments.len();
        self.after_session_change();
        if kept > 0 {
            self.info(format!(
                "deleted {removed} comments ({kept} forge-owned kept)"
            ));
        } else {
            self.info(format!("deleted {removed} comments"));
        }
    }

    fn jump_to_selected_comment(&mut self) {
        let Some(Modal::Comments { entries, cursor }) = self.modal.take() else {
            return;
        };
        let Some(entry) = entries.get(cursor).cloned() else {
            return;
        };
        if self.diff.is_none() {
            self.open_working_tree_diff(None);
        }
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.ensure_rows(&self.review);
        let model = diff
            .commit_model
            .clone()
            .unwrap_or_else(|| self.review.model().clone());
        let Some(file_index) = model.files.iter().position(|f| f.path == entry.file) else {
            self.info("comment file is not in this diff");
            return;
        };
        if diff.selected != file_index {
            diff.selected = file_index;
            diff.invalidate();
            diff.ensure_rows(&self.review);
        }
        let session = self.review.session_for(&diff.source);
        let target = diff.rows().iter().position(|row| {
            matches!(row, super::diff::DiffRow::Comment { comment, line: 0, .. }
                if session.comments.get(*comment).is_some_and(|c| c.id == entry.comment_id))
        });
        if let Some(row) = target {
            diff.cursor = row;
            diff.focus = super::Pane::Diff;
        }
        if self.screen() != super::Screen::Diff {
            self.push_screen(super::Screen::Diff);
        }
    }

    pub(super) fn handle_branch_list_key(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(Modal::BranchList {
                    branches, cursor, ..
                }) = self.modal.as_mut()
                {
                    *cursor = (*cursor + 1).min(branches.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(Modal::BranchList { cursor, .. }) = self.modal.as_mut() {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Enter => self.submit_branch_list(),
            _ => {}
        }
    }

    pub(super) fn submit_branch_list(&mut self) {
        let Some(Modal::BranchList {
            branches,
            cursor,
            action,
        }) = self.modal.take()
        else {
            return;
        };
        let Some(name) = branches.get(cursor).map(|b| b.name.clone()) else {
            return;
        };
        self.message = None;
        match action {
            BranchAction::Checkout => {
                self.vcs_op(|vcs| vcs.checkout(&name));
                if self.message.is_none() {
                    self.info(format!("checked out {name}"));
                }
            }
            BranchAction::Delete => {
                self.modal = Some(Modal::Confirm {
                    message: format!("Delete branch {name}?"),
                    on_confirm: PendingOp::DeleteBranch(name),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::super::{App, Modal, Pane, Screen};
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;
    use diffler_core::session::Anchor;

    fn press(app: &mut App, code: KeyCode) {
        app.handle(crate::event::AppEvent::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        )));
    }

    #[test]
    fn comments_overview_walks_and_jumps_to_the_comment() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.review.session.add_comment(
            "me",
            Anchor {
                file: "src/lib.rs".into(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "tighten this",
        );

        press(&mut app, KeyCode::Char('C'));
        let Some(Modal::Comments { entries, cursor }) = &app.modal else {
            panic!("overview modal open, got {:?}", app.modal);
        };
        assert_eq!(*cursor, 0);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].label.contains("src/lib.rs:2"),
            "{}",
            entries[0].label
        );
        assert!(entries[0].label.contains("tighten this"));

        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_none());
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff open");
        assert_eq!(diff.focus, Pane::Diff);
        assert!(
            matches!(
                diff.rows().get(diff.cursor),
                Some(super::super::diff::DiffRow::Comment { line: 0, .. })
            ),
            "cursor sits on the comment header row"
        );
    }

    #[test]
    fn d_vanishes_a_local_comment_and_capital_d_wipes_the_review() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        for (line, body) in [(1, "first"), (2, "second"), (3, "third")] {
            app.review.session.add_comment(
                "me",
                Anchor {
                    file: "src/lib.rs".into(),
                    line: Some(line),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                body,
            );
        }
        press(&mut app, KeyCode::Char('C'));
        press(&mut app, KeyCode::Char('d'));
        let Some(Modal::Comments { entries, .. }) = &app.modal else {
            panic!("overview stays open after a single delete");
        };
        assert_eq!(entries.len(), 2, "one comment vanished");
        assert_eq!(app.review.session.comments.len(), 2);

        press(&mut app, KeyCode::Char('D'));
        assert!(
            matches!(app.modal, Some(Modal::Confirm { .. })),
            "delete-all asks first"
        );
        press(&mut app, KeyCode::Char('y'));
        assert!(app.review.session.comments.is_empty(), "started fresh");
    }

    #[test]
    fn forge_owned_comments_refuse_local_deletion() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.review.session.add_comment(
            "alice",
            Anchor {
                file: "src/lib.rs".into(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "remote",
        );
        app.review.session.comments[0].remote_id = Some("9".into());
        let id = app.review.session.comments[0].id.clone();
        assert!(!app.delete_comment_by_id(&id));
        assert_eq!(app.review.session.comments.len(), 1);
        // delete-all keeps it too
        app.delete_all_comments();
        assert_eq!(app.review.session.comments.len(), 1);
    }

    #[test]
    fn overview_without_comments_just_says_so() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        press(&mut app, KeyCode::Char('C'));
        assert!(app.modal.is_none());
    }
}
