//! Modal and input handling, including the branch prompts.

use crossterm::event::{KeyCode, KeyEvent};

use std::path::Path;

use super::fuzzy::{FuzzyKey, FuzzyList, branch_haystack, name_haystack, rev_haystack, selected};
use super::text_edit;
use super::{App, BranchAction, Flow, InputOp, Modal, PendingOp, RevChoice};

impl App {
    pub(super) fn handle_modal_key(&mut self, key: &KeyEvent) -> Flow {
        match &self.modal {
            Some(Modal::Confirm { .. }) => match key.code {
                KeyCode::Char('y') => self.confirm_modal(),
                KeyCode::Char('n') | KeyCode::Esc => self.modal = None,
                _ => {}
            },
            Some(Modal::Input { .. }) => self.handle_input_key(key),
            Some(Modal::CreatePr { .. }) => return self.handle_create_pr_key(key),
            Some(Modal::ReviewVerdict { number, .. }) => {
                use crate::ci::ReviewVerdict;
                let number = *number;
                let verdict = match key.code {
                    KeyCode::Char('a') => Some(ReviewVerdict::Approve),
                    KeyCode::Char('x') => Some(ReviewVerdict::RequestChanges),
                    KeyCode::Char('c') => Some(ReviewVerdict::Comment),
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.modal = None;
                        None
                    }
                    _ => None,
                };
                if let Some(verdict) = verdict {
                    self.modal = None;
                    self.pr_review_verdict_chosen(number, verdict);
                }
            }
            Some(Modal::BranchList { .. }) => self.handle_branch_list_key(key),
            Some(Modal::PrBase { .. }) => self.handle_pr_base_key(key),
            Some(Modal::RevList { .. }) => self.handle_rev_list_key(key),
            Some(Modal::Palette { .. }) => return self.handle_palette_key(key),
            Some(Modal::FilePicker { .. }) => return self.handle_file_picker_key(key),
            Some(Modal::Themes { .. }) => self.handle_theme_key(key),
            Some(Modal::RemoteList { .. }) => self.handle_remote_list_key(key),
            Some(Modal::PullDiverged { .. }) => self.handle_pull_diverged_key(key),
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
            PendingOp::DeleteComment(id) => {
                self.delete_comment_by_id(&id);
            }
            PendingOp::RunGit { label, argv } => self.queue_network(label, argv),
            PendingOp::ForcePull { .. } => self.queue_network(
                "reset --hard",
                vec![
                    "git".to_owned(),
                    "reset".to_owned(),
                    "--hard".to_owned(),
                    "@{u}".to_owned(),
                ],
            ),
        }
    }

    pub(super) fn handle_input_key(&mut self, key: &KeyEvent) {
        let Some(Modal::Input { buffer, cursor, .. }) = self.modal.as_mut() else {
            return;
        };
        match text_edit::apply(buffer, cursor, key) {
            text_edit::Edit::Consumed => {}
            text_edit::Edit::Submit => self.submit_input(),
            text_edit::Edit::Cancel => self.cancel_input(),
        }
    }

    /// The pointer over an open dialog: the wheel walks the rows, a click puts
    /// the selection under it, and a second click on the same row takes it.
    pub(super) fn handle_modal_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        let row = mouse.row;
        match mouse.kind {
            MouseEventKind::ScrollDown => self.step_modal(true),
            MouseEventKind::ScrollUp => self.step_modal(false),
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(index) = self.modal_hits.and_then(|hits| hits.index_at(row)) else {
                    return;
                };
                let repeat = self.register_click_is_double(mouse.column, row);
                if self.point_modal_at(index) && repeat {
                    self.activate_modal();
                }
            }
            MouseEventKind::Down(MouseButton::Right) => self.cancel_modal(),
            _ => {}
        }
    }

    /// Move the selection one row. The create form steps its field; a list
    /// walks its matches.
    fn step_modal(&mut self, forward: bool) {
        match self.modal.as_mut() {
            Some(Modal::CreatePr { draft }) => draft.field = draft.field.step(forward),
            other => {
                let Some(list) = other.and_then(Modal::list_mut) else {
                    return;
                };
                list.step_selection(forward);
            }
        }
    }

    /// Put the selection on `index`, reporting whether it was already there
    /// (a click on the selected row is the one that activates).
    fn point_modal_at(&mut self, index: usize) -> bool {
        match self.modal.as_mut() {
            Some(Modal::CreatePr { draft }) => {
                let Some(field) = crate::app::pr_create::PrField::ORDER.get(index).copied() else {
                    return false;
                };
                let same = draft.field == field;
                draft.field = field;
                same
            }
            other => {
                let Some(list) = other.and_then(Modal::list_mut) else {
                    return false;
                };
                let same = list.selected == index;
                list.selected = index.min(list.matches.len().saturating_sub(1));
                same
            }
        }
    }

    /// Take the selected row, as Enter would.
    fn activate_modal(&mut self) {
        let enter = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        self.handle_modal_key(&enter);
    }

    fn cancel_modal(&mut self) {
        let esc = KeyEvent::new(KeyCode::Esc, crossterm::event::KeyModifiers::NONE);
        self.handle_modal_key(&esc);
    }

    pub(super) fn handle_create_pr_key(&mut self, key: &KeyEvent) -> Flow {
        let Some(Modal::CreatePr { draft }) = self.modal.as_mut() else {
            return Flow::Continue;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => draft.field = draft.field.step(true),
            KeyCode::Char('k') | KeyCode::Up => draft.field = draft.field.step(false),
            KeyCode::Char('d') => draft.draft = !draft.draft,
            KeyCode::Char('c') => self.create_pr_submit(),
            KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
            KeyCode::Enter => {
                let field = draft.field;
                let Some(Modal::CreatePr { draft }) = self.modal.take() else {
                    return Flow::Continue;
                };
                self.edit_pr_field(draft, field);
            }
            _ => {}
        }
        Flow::Continue
    }

    fn edit_pr_field(
        &mut self,
        draft: Box<crate::app::pr_create::PrDraft>,
        field: crate::app::pr_create::PrField,
    ) {
        use crate::app::pr_create::PrField;
        match field {
            PrField::Draft => {
                let mut draft = draft;
                draft.draft = !draft.draft;
                self.modal = Some(Modal::CreatePr { draft });
            }
            PrField::Body => {
                let template = draft.body.clone();
                let restore = draft.clone();
                // a PR body is markdown on every forge, and the extension is
                // what the editor reads the filetype from
                let queued =
                    self.queue_message_editor("PR_EDITMSG.md", template, move |msg_path| {
                        crate::editor::EditorPurpose::PrBody { msg_path, draft }
                    });
                if !queued {
                    self.modal = Some(Modal::CreatePr { draft: restore });
                }
            }
            PrField::Base => self.open_pr_base_list(draft),
            PrField::Title => {
                let buffer = draft.title.clone();
                self.open_input(
                    "Title".to_owned(),
                    buffer,
                    InputOp::PrField { draft, field },
                );
            }
        }
    }

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

    /// An empty buffer submits as a cancel: comments and replies must say
    /// something to be worth persisting.
    /// Leave the input. A field of the create form hands its draft back, so
    /// one abandoned edit keeps the rest of the form.
    pub(super) fn cancel_input(&mut self) {
        if let Some(Modal::Input {
            on_submit: InputOp::PrField { draft, .. },
            ..
        }) = self.modal.take()
        {
            self.modal = Some(Modal::CreatePr { draft });
        }
    }

    pub(super) fn submit_input(&mut self) {
        let Some(Modal::Input {
            buffer, on_submit, ..
        }) = self.modal.take()
        else {
            return;
        };
        let body = buffer.trim();
        // the review summary is optional; everything else needs content
        if body.is_empty()
            && !matches!(
                on_submit,
                InputOp::ReviewBody { .. } | InputOp::PrField { .. }
            )
        {
            return;
        }
        match on_submit {
            InputOp::PrField { mut draft, field } => {
                use crate::app::pr_create::PrField;
                if !body.is_empty() {
                    match field {
                        PrField::Base => body.clone_into(&mut draft.base),
                        _ => body.clone_into(&mut draft.title),
                    }
                }
                self.modal = Some(Modal::CreatePr { draft });
            }
            InputOp::ReviewBody { number, verdict } => {
                let body = body.to_owned();
                self.queue_pr_review(number, verdict, &body);
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
                let mut list = FuzzyList::default();
                list.rerank(&branch_haystack(&branches));
                self.modal = Some(Modal::BranchList {
                    branches,
                    list,
                    action,
                });
            }
            Err(err) => {
                self.modal = None;
                self.error(err.to_string());
            }
        }
    }

    /// Pick what to diff the working tree against. Enter opens the three-dot
    /// review for the chosen revision.
    pub(super) fn open_rev_list(&mut self, title: &'static str, entries: Vec<RevChoice>) {
        if entries.is_empty() {
            self.modal = None;
            self.info("nothing to diff against");
            return;
        }
        let mut list = FuzzyList::default();
        list.rerank(&rev_haystack(&entries));
        self.modal = Some(Modal::RevList {
            title,
            entries,
            list,
        });
    }

    /// Pick the branch a pull request merges into. The remote's branches are
    /// what a forge accepts, so those lead; a repo whose remote refs are not
    /// fetched falls back to the local ones.
    pub(super) fn open_pr_base_list(&mut self, draft: Box<crate::app::pr_create::PrDraft>) {
        let names = self.base_candidates(&draft.head, &draft.base);
        if names.is_empty() {
            let buffer = draft.base.clone();
            self.open_input(
                "Base branch".to_owned(),
                buffer,
                InputOp::PrField {
                    draft,
                    field: crate::app::pr_create::PrField::Base,
                },
            );
            return;
        }
        let mut list = FuzzyList::default();
        list.rerank(&name_haystack(&names));
        self.modal = Some(Modal::PrBase { names, list, draft });
    }

    /// Branch names a pull request can merge into, the current base first.
    fn base_candidates(&self, head: &str, base: &str) -> Vec<String> {
        let Ok(all) = self.review.vcs.all_branches() else {
            return Vec::new();
        };
        let remotes = self.review.vcs.remotes().unwrap_or_default();
        let mut names: Vec<String> = all
            .iter()
            .filter_map(|name| {
                remotes
                    .iter()
                    .find_map(|remote| name.strip_prefix(&format!("{remote}/")))
            })
            .filter(|name| *name != head && *name != "HEAD")
            .map(str::to_owned)
            .collect();
        names.sort_unstable();
        names.dedup();
        if let Some(at) = names.iter().position(|name| name == base) {
            names.remove(at);
        }
        if !base.is_empty() {
            names.insert(0, base.to_owned());
        }
        names
    }

    pub(super) fn handle_pr_base_key(&mut self, key: &KeyEvent) {
        let Some(Modal::PrBase { names, list, .. }) = self.modal.as_mut() else {
            return;
        };
        match list.feed(key) {
            FuzzyKey::Submit => self.submit_pr_base(),
            FuzzyKey::Cancel => self.close_pr_base(None),
            FuzzyKey::Edited => {
                let haystack = name_haystack(names);
                list.rerank(&haystack);
            }
            _ => {}
        }
    }

    fn submit_pr_base(&mut self) {
        let Some(Modal::PrBase { names, list, .. }) = &self.modal else {
            return;
        };
        // a query matching nothing keeps the dialog open, like fzf
        let Some(name) = selected(list, names).cloned() else {
            return;
        };
        self.close_pr_base(Some(name));
    }

    /// Back to the form, carrying the pick when there was one.
    fn close_pr_base(&mut self, chosen: Option<String>) {
        let Some(Modal::PrBase { mut draft, .. }) = self.modal.take() else {
            return;
        };
        if let Some(name) = chosen {
            draft.base = name;
        }
        self.modal = Some(Modal::CreatePr { draft });
    }

    pub(super) fn handle_rev_list_key(&mut self, key: &KeyEvent) {
        let Some(Modal::RevList { entries, list, .. }) = self.modal.as_mut() else {
            return;
        };
        match list.feed(key) {
            FuzzyKey::Submit => self.submit_rev_list(),
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited => {
                let haystack = rev_haystack(entries);
                list.rerank(&haystack);
            }
            _ => {}
        }
    }

    fn submit_rev_list(&mut self) {
        // a query matching nothing keeps the dialog open, like fzf
        let Some(Modal::RevList { entries, list, .. }) = &self.modal else {
            return;
        };
        let Some(rev) = selected(list, entries).map(|choice| choice.rev.clone()) else {
            return;
        };
        self.modal = None;
        self.open_against_diff(&rev);
    }

    /// Delete one comment outright. Forge-owned comments decline: the next
    /// sync would just re-import them.
    pub(super) fn delete_comment_by_id(&mut self, id: &str) -> bool {
        let source = self.active_review_source();
        let session = self.review.session_for_mut(&source);
        let remote = session
            .comments
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.remote_id.clone());
        // a forge-owned comment deletes on the forge first; the local copy
        // goes when the forge confirms (the forge 403s on others' comments)
        if let Some(remote_id) = remote {
            if let diffler_core::source::ReviewSource::Pr { number } = source {
                self.queue_pr_comment_delete(number, id, &remote_id);
                self.info("deleting the comment on the forge…");
            } else {
                self.info("forge comment: open the PR review to delete it");
            }
            return false;
        }
        if !session.delete_comment(id) {
            return false;
        }
        self.after_session_change();
        true
    }

    pub(super) fn handle_palette_key(&mut self, key: &KeyEvent) -> Flow {
        let (commands, haystack) = self.command_index_haystack();
        let Some(Modal::Palette { list }) = self.modal.as_mut() else {
            return Flow::Continue;
        };
        match list.feed(key) {
            FuzzyKey::Submit => {
                // a query matching nothing keeps the palette open, like fzf
                if let Some(action) = selected(list, &commands).map(|c| c.action) {
                    self.modal = None;
                    return self.dispatch(action);
                }
            }
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited => list.rerank(&haystack),
            _ => {}
        }
        Flow::Continue
    }

    pub(super) fn handle_theme_key(&mut self, key: &KeyEvent) {
        let Some(Modal::Themes { list }) = self.modal.as_mut() else {
            return;
        };
        match list.feed(key) {
            FuzzyKey::Submit => self.submit_theme(),
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited => list.rerank(&crate::theme::names()),
            _ => {}
        }
    }

    fn submit_theme(&mut self) {
        // a query matching nothing keeps the dialog open, like fzf
        let names = crate::theme::names();
        let Some(Modal::Themes { list }) = &self.modal else {
            return;
        };
        let Some(name) = selected(list, &names).cloned() else {
            return;
        };
        self.modal = None;
        self.apply_theme(&name);
    }

    pub(super) fn handle_remote_list_key(&mut self, key: &KeyEvent) {
        let Some(Modal::RemoteList { remotes, list, .. }) = self.modal.as_mut() else {
            return;
        };
        match list.feed(key) {
            FuzzyKey::Submit => self.submit_remote_list(),
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited => {
                let haystack = remotes.clone();
                list.rerank(&haystack);
            }
            _ => {}
        }
    }

    fn submit_remote_list(&mut self) {
        let Some(Modal::RemoteList {
            remotes,
            list,
            purpose,
        }) = &self.modal
        else {
            return;
        };
        let purpose = *purpose;
        let Some(remote) = selected(list, remotes).cloned() else {
            return;
        };
        self.modal = None;
        self.remote_chosen(&remote, purpose);
    }

    pub(super) fn handle_pull_diverged_key(&mut self, key: &KeyEvent) {
        let Some(Modal::PullDiverged { upstream }) = &self.modal else {
            return;
        };
        match key.code {
            KeyCode::Char('r') => {
                self.modal = None;
                self.pull_rebase();
            }
            KeyCode::Char('m') => {
                self.modal = None;
                self.pull_merge();
            }
            KeyCode::Char('f') => {
                let upstream = upstream.clone();
                self.modal = Some(Modal::Confirm {
                    message: format!(
                        "Discard your local commits and uncommitted changes, resetting hard to {upstream}?"
                    ),
                    on_confirm: PendingOp::ForcePull { upstream },
                });
            }
            KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
            _ => {}
        }
    }

    pub(super) fn handle_branch_list_key(&mut self, key: &KeyEvent) {
        let Some(Modal::BranchList { branches, list, .. }) = self.modal.as_mut() else {
            return;
        };
        match list.feed(key) {
            FuzzyKey::Submit => self.submit_branch_list(),
            FuzzyKey::Cancel => self.modal = None,
            FuzzyKey::Edited => {
                let haystack = branch_haystack(branches);
                list.rerank(&haystack);
            }
            _ => {}
        }
    }

    pub(super) fn submit_branch_list(&mut self) {
        // a query matching nothing keeps the dialog open, like fzf
        let Some(Modal::BranchList {
            branches,
            list,
            action,
        }) = &self.modal
        else {
            return;
        };
        let action = *action;
        let Some(name) = selected(list, branches).map(|b| b.name.clone()) else {
            return;
        };
        self.modal = None;
        self.message = None;
        match action {
            BranchAction::Checkout => self.checkout_branch(&name),
            BranchAction::Delete => {
                self.modal = Some(Modal::Confirm {
                    message: format!("Delete branch {name}?"),
                    on_confirm: PendingOp::DeleteBranch(name),
                });
            }
        }
    }

    /// Check out `name`, shared by the branch picker and a `<cr>` on a branch
    /// row in the status screen's Branches section. Checking out the branch
    /// already active is a no-op info message, not a git error.
    pub(super) fn checkout_branch(&mut self, name: &str) {
        if self.head.branch.as_deref() == Some(name) {
            self.info(format!("already on {name}"));
            return;
        }
        self.message = None;
        let owned = name.to_owned();
        self.vcs_op(move |vcs| vcs.checkout(&owned));
        if self.message.is_none() {
            self.info(format!("checked out {name}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::super::{App, BranchAction, Modal, Pane, Screen};
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;
    use diffler_core::session::Anchor;

    fn press(app: &mut App, code: KeyCode) {
        app.handle(crate::event::AppEvent::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        )));
    }

    fn press_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle(crate::event::AppEvent::Key(KeyEvent::new(code, modifiers)));
    }

    fn chord(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle(crate::event::AppEvent::Key(KeyEvent::new(code, modifiers)));
    }

    /// The open input modal's `(buffer, cursor)`.
    fn input_state(app: &App) -> (String, usize) {
        let Some(Modal::Input { buffer, cursor, .. }) = &app.modal else {
            panic!("input modal open, got {:?}", app.modal);
        };
        (buffer.clone(), *cursor)
    }

    fn input_app(prefill: &str) -> App {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_input(
            "New branch".to_owned(),
            prefill.to_owned(),
            super::super::InputOp::CreateBranch { checkout: false },
        );
        app
    }

    #[test]
    fn readline_line_motions_and_kills() {
        let mut app = input_app("fix the name");
        // ctrl-a → start, ctrl-e → end
        chord(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app).1, 0);
        chord(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app).1, 12);
        // ctrl-u kills to line start
        chord(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app), (String::new(), 0));
    }

    #[test]
    fn readline_motions_are_line_scoped_in_multiline_buffers() {
        let mut app = input_app("first line\nsecond here");
        // cursor opens at the very end; ctrl-a stops at the second line's start
        chord(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app).1, 11);
        // ctrl-k kills only the second line
        chord(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app).0, "first line\n");
    }

    #[test]
    fn readline_word_deletion_and_motion() {
        let mut app = input_app("delete the last word");
        // alt-backspace eats "word", ctrl-w eats "last "
        chord(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(input_state(&app).0, "delete the last ");
        chord(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app).0, "delete the ");
        // alt-b steps back over "the"; alt-d deletes the word, keeping the
        // spaces on both sides as readline does
        chord(&mut app, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(input_state(&app).1, 7);
        chord(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!(input_state(&app).0, "delete  ");
    }

    #[test]
    fn control_chords_never_insert_their_letter() {
        let mut app = input_app("");
        chord(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        chord(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(input_state(&app).0, "", "chords must not type text");
        press(&mut app, KeyCode::Char('A'));
        assert_eq!(input_state(&app).0, "A", "plain shift still types");
    }

    #[test]
    fn altgr_chars_still_type_text() {
        // Windows reports AltGr as ctrl+alt together; the produced char is text
        let mut app = input_app("");
        chord(
            &mut app,
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(input_state(&app).0, "@");
    }

    #[test]
    fn ctrl_h_deletes_backward_and_ctrl_k_joins_lines() {
        let mut app = input_app("ab");
        chord(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app), ("a".to_owned(), 1));

        let mut app = input_app("first\nsecond");
        // park at the end of the first line, then kill the newline
        for _ in 0..7 {
            chord(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
        }
        assert_eq!(input_state(&app).1, 5);
        chord(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app).0, "firstsecond");
    }

    #[test]
    fn readline_ops_stay_on_char_boundaries_with_multibyte_text() {
        let mut app = input_app("h\u{e9}llo \u{4e16}\u{754c} \u{1f44d}");
        // ctrl-w eats the emoji word, alt-b crosses the CJK word
        chord(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app).0, "h\u{e9}llo \u{4e16}\u{754c} ");
        chord(&mut app, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(input_state(&app).1, 6);
        chord(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!(input_state(&app).0, "h\u{e9}llo  ");
        chord(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app).0, " ");
        // forward char motion and delete at the end are clamped no-ops
        chord(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        chord(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(input_state(&app), (" ".to_owned(), 1));
    }

    #[test]
    fn comments_overview_walks_and_jumps_to_the_comment() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.review.session.add_comment(
            Anchor {
                file: "src/lib.rs".into(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "me",
            "tighten this",
        );

        press(&mut app, KeyCode::Char('C'));
        assert!(app.modal.is_none(), "the sidebar replaces the old dialog");
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff open");
        assert!(diff.comments_open());
        assert_eq!(diff.focus, Pane::Comments);
        assert!(
            matches!(
                diff.rows().get(diff.cursor),
                Some(super::super::diff::DiffRow::Comment { line: 0, .. })
            ),
            "opening seats the diff cursor on the selected comment"
        );

        // the pane's own verbs reach the selected comment from the sidebar
        press(&mut app, KeyCode::Char('d'));
        assert!(
            matches!(app.modal, Some(Modal::Confirm { .. })),
            "delete asks first, from the sidebar"
        );
        press(&mut app, KeyCode::Char('y'));
        assert!(app.review.session.comments.is_empty());
    }

    #[test]
    fn palette_runs_the_best_match_on_enter() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        press_with(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(matches!(app.modal, Some(Modal::Palette { .. })));
        for c in "help".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.modal, Some(Modal::Help), "palette dispatched help");
    }

    #[test]
    fn enter_on_a_query_matching_nothing_keeps_the_palette_open() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        press_with(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        for c in "zzzzqx".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::Palette { .. })));
    }

    #[test]
    fn theme_picker_switches_the_theme_live() {
        use crate::theme::Theme;
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        assert_eq!(app.theme, Theme::github_dark());
        press(&mut app, KeyCode::Char('T'));
        assert!(
            matches!(app.modal, Some(Modal::Themes { .. })),
            "T opens the theme picker"
        );
        press(&mut app, KeyCode::Tab);
        for c in "dracula".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_none());
        assert_eq!(app.theme, Theme::dracula());
    }

    #[test]
    fn branch_list_checks_out_the_best_fuzzy_match() {
        let fixture = standard_fixture();
        fixture.branch("feat/topic");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_branch_list(BranchAction::Checkout);
        press(&mut app, KeyCode::Tab);
        for c in "topi".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(app.modal.is_none());
        assert_eq!(
            app.review.vcs.head().expect("head").branch.as_deref(),
            Some("feat/topic")
        );
    }

    #[test]
    fn forge_owned_comments_refuse_local_deletion() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.review.session.add_comment(
            Anchor {
                file: "src/lib.rs".into(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "alice",
            "remote",
        );
        app.review.session.comments[0].remote_id = Some("9".into());
        let id = app.review.session.comments[0].id.clone();
        assert!(!app.delete_comment_by_id(&id));
        assert_eq!(app.review.session.comments.len(), 1);
    }
}
