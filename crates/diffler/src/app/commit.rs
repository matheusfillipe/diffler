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

    /// `c c`: open the editor on a commit message when something is staged.
    pub(crate) fn commit_flow(&mut self) {
        let staged = &self.review.status.staged.files;
        if staged.is_empty() {
            self.info("nothing staged");
            return;
        }
        let template = editor::commit_template(staged);
        self.queue_message_editor("COMMIT_EDITMSG", template, |msg_path| {
            EditorPurpose::Commit { msg_path }
        });
    }

    /// `c e`: extend HEAD with the staged index, reusing its message.
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
        self.queue_message_editor("COMMIT_EDITMSG", template, move |msg_path| {
            EditorPurpose::Amend {
                msg_path,
                use_index,
            }
        });
    }

    /// Write `template` to `file_name` in the git dir and queue the editor on
    /// it. We ask libgit2 for the gitdir so linked worktrees work. Returns
    /// whether the editor was queued, so a caller can restore its state.
    pub(super) fn queue_message_editor(
        &mut self,
        file_name: &str,
        template: String,
        purpose: impl FnOnce(std::path::PathBuf) -> EditorPurpose,
    ) -> bool {
        let git_dir = match self.review.vcs.git_dir() {
            Ok(dir) => dir,
            Err(err) => {
                self.error(err.to_string());
                return false;
            }
        };
        let msg_path = git_dir.join(file_name);
        if let Err(err) = std::fs::write(&msg_path, template) {
            self.error(format!("cannot write {}: {err}", msg_path.display()));
            return false;
        }
        let cmd = editor::command_for(&self.editor_command(), &msg_path, None);
        self.pending_editor = Some(EditorRequest {
            cmd,
            purpose: purpose(msg_path),
        });
        true
    }

    /// Write `template` to a uniquely named temp file and queue the editor on
    /// it. [`Self::take_scratch_edit`] removes the file.
    pub(super) fn queue_scratch_editor(
        &mut self,
        template: &str,
        purpose: impl FnOnce(std::path::PathBuf) -> EditorPurpose,
    ) -> bool {
        let path = std::env::temp_dir().join(format!("diffler-edit-{}.md", uuid::Uuid::new_v4()));
        if let Err(err) = std::fs::write(&path, template) {
            self.error(format!("cannot write {}: {err}", path.display()));
            return false;
        }
        let cmd = editor::command_for(&self.editor_command(), &path, None);
        self.pending_editor = Some(EditorRequest {
            cmd,
            purpose: purpose(path),
        });
        true
    }

    /// Read a scratch editor's file back and remove it, whatever the outcome.
    /// `None` for a cancelled edit, a failed editor, or an unreadable file, so
    /// the caller keeps its buffer.
    pub(super) fn take_scratch_edit(
        &mut self,
        path: &Path,
        outcome: Result<bool, String>,
    ) -> Option<String> {
        let read = match outcome {
            Ok(true) => std::fs::read_to_string(path),
            // a non-zero editor exit (e.g. vim's :cq) cancels the edit
            Ok(false) => {
                self.info("edit aborted");
                let _ = std::fs::remove_file(path);
                return None;
            }
            Err(err) => {
                self.error(format!("editor failed: {err}"));
                let _ = std::fs::remove_file(path);
                return None;
            }
        };
        let _ = std::fs::remove_file(path);
        match read {
            Ok(text) => Some(text.strip_suffix('\n').unwrap_or(&text).to_owned()),
            Err(err) => {
                self.error(format!("cannot read {}: {err}", path.display()));
                None
            }
        }
    }

    pub(super) fn apply_text_box_edit(
        &mut self,
        target: crate::editor::TextBoxTarget,
        text: String,
    ) {
        use crate::editor::TextBoxTarget;
        match target {
            TextBoxTarget::Composer => {
                let Some(composer) = self.diff.as_mut().and_then(|d| d.composer.as_mut()) else {
                    return;
                };
                composer.buffer = text;
                composer.cursor = composer.buffer.chars().count();
                if let Some(diff) = self.diff.as_mut() {
                    diff.mark_reflow();
                    diff.ensure_rows(&self.review);
                }
            }
            TextBoxTarget::Input => {
                let Some(super::Modal::Input { buffer, cursor, .. }) = self.modal.as_mut() else {
                    return;
                };
                *buffer = text;
                *cursor = buffer.chars().count();
            }
        }
    }

    /// Run the backend amend and report. `message` `None` reuses HEAD's
    /// message (extend); `use_index` folds the staged index in.
    pub(super) fn apply_amend(&mut self, message: Option<&str>, use_index: bool) {
        match self.review.vcs.amend(message, use_index) {
            Ok(oid) => {
                // an extend reuses HEAD's message, so its subject is the one
                // already on screen
                let subject = message
                    .and_then(|text| text.lines().next())
                    .unwrap_or(&self.head.subject)
                    .to_owned();
                self.queue_refresh();
                let oid7 = oid.get(..7).unwrap_or(&oid).to_owned();
                if self.message.is_none() {
                    self.info(format!("amended {oid7} {subject}"));
                }
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    /// `outcome` is the editor's success, or the spawn failure message.
    pub fn editor_finished(&mut self, purpose: EditorPurpose, outcome: Result<bool, String>) {
        self.message = None;
        match purpose {
            EditorPurpose::Commit { msg_path } => self.finish_commit(&msg_path, outcome),
            EditorPurpose::Amend {
                msg_path,
                use_index,
            } => self.finish_amend(&msg_path, use_index, outcome),
            EditorPurpose::PrBody {
                msg_path,
                mut draft,
                field,
            } => {
                if let Some(text) = self.take_scratch_edit(&msg_path, outcome) {
                    match field {
                        crate::app::pr_create::PrTextField::Title => draft.title = text,
                        crate::app::pr_create::PrTextField::Body => draft.body = text,
                    }
                }
                self.modal = Some(super::Modal::CreatePr { draft });
            }
            EditorPurpose::TextBox { path, target } => {
                if let Some(text) = self.take_scratch_edit(&path, outcome) {
                    self.apply_text_box_edit(target, text);
                }
            }
            EditorPurpose::OpenFile { path } => {
                if let Err(err) = outcome {
                    self.error(format!("editor failed: {err}"));
                }
                self.queue_refresh();
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
                self.queue_refresh();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    #[test]
    fn committing_reports_the_ordinary_commit_message() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());

        let msg_path = app.review.repo_root.join("MSG");
        std::fs::write(&msg_path, "a plain commit\n").expect("write commit message");
        app.finish_commit(&msg_path, Ok(true));

        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.starts_with("committed ")),
            "{:?}",
            app.message
        );
    }
}
