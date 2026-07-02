//! Commit, amend, and editor flows.

use std::path::Path;

use super::App;
use crate::editor::{self, EditorPurpose, EditorRequest};

impl App {
    /// Editor command at the point of use: config beats `$DIFFLER_EDITOR`
    /// beats `$EDITOR` beats `vi`.
    pub(super) fn editor_command(&self) -> String {
        editor::resolve(
            self.config.editor.command.as_deref(),
            std::env::var("DIFFLER_EDITOR").ok().as_deref(),
            std::env::var("EDITOR").ok().as_deref(),
        )
    }

    /// Queue the editor on a repo-relative path, optionally at a line.
    pub(crate) fn request_editor(&mut self, path: &str, line: Option<u32>) {
        let absolute = self.review.repo_root.join(path);
        let cmd = editor::command_for(&self.editor_command(), &absolute, line);
        self.pending_editor = Some(EditorRequest {
            cmd,
            purpose: EditorPurpose::OpenFile {
                path: path.to_owned(),
            },
        });
    }

    // --- network ops (push/pull/fetch) ---

    /// Queue a network git op: resolve its argv from the backend and set
    /// `pending_git`, plus a "running …" status so the next draw shows it. The
    /// main loop runs the process in the background and reports back through
    /// [`AppEvent::GitDone`], so the event loop never freezes on the network.
    /// `c c`: straight to the editor on a gitcommit-style message file when
    /// something is staged.
    pub(crate) fn commit_flow(&mut self) {
        let staged = &self.review.status.staged.files;
        if staged.is_empty() {
            self.info("nothing staged");
            return;
        }
        let template = editor::commit_template(staged);
        self.queue_message_editor(template, |msg_path| EditorPurpose::Commit { msg_path });
    }

    /// `c e`: extend HEAD with the staged index, reusing its message — no
    /// editor. Refused on an empty index (nothing to add) or unborn branch.
    pub(crate) fn commit_extend(&mut self) {
        if self.review.status.staged.files.is_empty() {
            self.info("nothing staged");
            return;
        }
        if self.head.oid7.is_empty() {
            self.info("no commit to extend");
            return;
        }
        self.apply_amend(None, true);
    }

    /// `c a`: amend HEAD via the editor on its message, folding the staged
    /// index into the new commit.
    pub(crate) fn commit_amend(&mut self) {
        self.amend_via_editor(true);
    }

    /// `c w`: reword HEAD via the editor on its message, keeping its tree.
    pub(crate) fn commit_reword(&mut self) {
        self.amend_via_editor(false);
    }

    pub(super) fn amend_via_editor(&mut self, use_index: bool) {
        if self.head.oid7.is_empty() {
            self.info("no commit to amend");
            return;
        }
        let existing = match self.review.vcs.head_message() {
            Ok(message) => message,
            Err(err) => {
                self.error(err.to_string());
                return;
            }
        };
        // a reword keeps HEAD's tree, so its template lists no staged files
        let staged: &[diffler_core::model::FileDiff] = if use_index {
            &self.review.status.staged.files
        } else {
            &[]
        };
        let template = editor::amend_template(&existing, staged);
        self.queue_message_editor(template, move |msg_path| EditorPurpose::Amend {
            msg_path,
            use_index,
        });
    }

    /// Write `template` to `COMMIT_EDITMSG` and queue the editor on it. The
    /// gitdir comes from libgit2 (not `<root>/.git`) so linked worktrees work.
    pub(super) fn queue_message_editor(
        &mut self,
        template: String,
        purpose: impl FnOnce(std::path::PathBuf) -> EditorPurpose,
    ) {
        let git_dir = match self.review.vcs.git_dir() {
            Ok(dir) => dir,
            Err(err) => {
                self.error(err.to_string());
                return;
            }
        };
        let msg_path = git_dir.join("COMMIT_EDITMSG");
        if let Err(err) = std::fs::write(&msg_path, template) {
            self.error(format!("cannot write {}: {err}", msg_path.display()));
            return;
        }
        let cmd = editor::command_for(&self.editor_command(), &msg_path, None);
        self.pending_editor = Some(EditorRequest {
            cmd,
            purpose: purpose(msg_path),
        });
    }

    /// Run the backend amend and report. `message` `None` reuses HEAD's
    /// message (extend); `use_index` folds the staged index in.
    pub(super) fn apply_amend(&mut self, message: Option<&str>, use_index: bool) {
        match self.review.vcs.amend(message, use_index) {
            Ok(oid) => {
                self.refresh();
                let subject = self.head.subject.clone();
                let oid7 = oid.get(..7).unwrap_or(&oid).to_owned();
                if self.message.is_none() {
                    self.info(format!("amended {oid7} {subject}"));
                }
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    /// Called by the main loop after the editor subprocess ended and the
    /// terminal is back. `outcome` is the editor's success, or the spawn
    /// failure message.
    pub fn editor_finished(&mut self, purpose: EditorPurpose, outcome: Result<bool, String>) {
        self.message = None;
        match purpose {
            EditorPurpose::Commit { msg_path } => self.finish_commit(&msg_path, outcome),
            EditorPurpose::Amend {
                msg_path,
                use_index,
            } => self.finish_amend(&msg_path, use_index, outcome),
            EditorPurpose::OpenFile { path } => {
                if let Err(err) = outcome {
                    self.error(format!("editor failed: {err}"));
                }
                self.refresh();
                if self.message.is_none() {
                    self.info(format!("edited {path}"));
                }
            }
        }
    }

    pub(super) fn finish_commit(&mut self, msg_path: &Path, outcome: Result<bool, String>) {
        match outcome {
            Err(err) => {
                self.error(format!("editor failed: {err}"));
                return;
            }
            // a non-zero editor exit (e.g. vim's :cq) aborts the commit
            Ok(false) => {
                self.info("commit aborted");
                return;
            }
            Ok(true) => {}
        }
        let raw = match std::fs::read_to_string(msg_path) {
            Ok(raw) => raw,
            Err(err) => {
                self.error(format!("cannot read {}: {err}", msg_path.display()));
                return;
            }
        };
        let Some(message) = editor::strip_commit_message(&raw) else {
            self.info("commit aborted");
            return;
        };
        match self.review.vcs.commit(&message) {
            Ok(oid) => {
                let subject = message.lines().next().unwrap_or_default().to_owned();
                let oid7 = oid.get(..7).unwrap_or(&oid).to_owned();
                self.refresh();
                if self.message.is_none() {
                    self.info(format!("committed {oid7} {subject}"));
                }
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    pub(super) fn finish_amend(
        &mut self,
        msg_path: &Path,
        use_index: bool,
        outcome: Result<bool, String>,
    ) {
        match outcome {
            Err(err) => {
                self.error(format!("editor failed: {err}"));
                return;
            }
            // a non-zero editor exit (e.g. vim's :cq) aborts the amend
            Ok(false) => {
                self.info("amend aborted");
                return;
            }
            Ok(true) => {}
        }
        let raw = match std::fs::read_to_string(msg_path) {
            Ok(raw) => raw,
            Err(err) => {
                self.error(format!("cannot read {}: {err}", msg_path.display()));
                return;
            }
        };
        let Some(message) = editor::strip_commit_message(&raw) else {
            self.info("amend aborted");
            return;
        };
        self.apply_amend(Some(&message), use_index);
    }
}
