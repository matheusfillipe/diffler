//! Application state and event handling. `App::handle` is a pure-ish state
//! transition (no terminal IO) so the whole shell is unit-testable; rendering
//! reads the state in `ui::draw`. Per-screen state and handlers live in the
//! `status`, `log`, and `diff` submodules.

mod diff;
mod log;
mod mcp;
mod status;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use diff::{CommentLine, DiffRow, DiffSource, DiffView, FileHighlights, comment_display};
pub use log::LogView;
pub use status::{Row, Section, StatusView};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use diffler_core::review::Review;
use diffler_core::session::Anchor;
use diffler_core::vcs::{BranchInfo, HeadInfo, Vcs, VcsError};

use crate::config::{Config, KeyPress, LoadedConfig};
use crate::editor::{self, EditorPurpose, EditorRequest};
use crate::event::AppEvent;
use crate::keymap::{self, Action, Context, Keymap, Resolved};
use crate::theme::Theme;

/// What the main loop should do after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// Screen stack entry. The per-screen state lives in `App::status`,
/// `App::log`, and `App::diff`; the stack only decides which one is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Status,
    Log,
    Diff,
}

impl Screen {
    fn context(self) -> Context {
        match self {
            Self::Status => Context::Status,
            Self::Diff => Context::Diff,
            Self::Log => Context::Log,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMessage {
    pub text: String,
    pub severity: Severity,
}

/// Deferred operation a modal confirms, kept as data (not a closure) so
/// `App::handle` stays a pure state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOp {
    Discard { path: String },
    DeleteBranch(String),
}

/// What an input modal does with its buffer on submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOp {
    Comment { anchor: Anchor },
    Reply { comment_id: String },
    CreateBranch { checkout: bool },
}

/// What selecting a branch in the branch list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchAction {
    Checkout,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    Confirm {
        message: String,
        on_confirm: PendingOp,
    },
    Input {
        title: String,
        buffer: String,
        /// Character index into `buffer`.
        cursor: usize,
        on_submit: InputOp,
    },
    /// Branch action popup (neogit `b`).
    Branch,
    /// Branch picker feeding `action` with the selected name.
    BranchList {
        branches: Vec<BranchInfo>,
        cursor: usize,
        action: BranchAction,
    },
    /// Keymap listing for the screen the popup opened over.
    Help,
}

struct Keymaps {
    status: Keymap,
    diff: Keymap,
    log: Keymap,
}

/// Pending multi-key sequences die after this many 250ms ticks.
const PENDING_TIMEOUT_TICKS: u8 = 4;
/// How long the post-refresh `↻` status-bar indicator stays up.
const REFRESH_FLASH_TICKS: u8 = 4;
/// Poll interval (in 250ms ticks) when the watcher is missing or broken.
const FALLBACK_REFRESH_TICKS: u32 = 20;

pub struct App {
    pub review: Review,
    pub head: HeadInfo,
    pub theme: Theme,
    pub config: Config,
    /// Author label stamped on comments and replies the human writes.
    pub author: String,
    pub screens: Vec<Screen>,
    pub status: StatusView,
    pub log: Option<LogView>,
    pub diff: Option<DiffView>,
    pub modal: Option<Modal>,
    pub message: Option<StatusMessage>,
    /// Escape sequence (OSC52 copy) the main loop writes raw to the
    /// terminal after the next draw, then clears.
    pub pending_osc: Option<String>,
    /// Editor subprocess the main loop runs with the terminal suspended,
    /// then reports back through [`App::editor_finished`].
    pub pending_editor: Option<EditorRequest>,
    /// Watcher health flag, set by `watch::spawn_watcher`. `None` (no
    /// watcher) counts as unhealthy: the tick fallback polls instead.
    pub watcher_healthy: Option<Arc<AtomicBool>>,
    /// Ticks left on the status-bar `↻` indicator after a repo change.
    pub refresh_flash: u8,
    /// Feedback epoch counter, bumped when the human sends feedback (`Z`)
    /// or touches a comment; the MCP `wait_for_feedback` long-poll holds
    /// receivers on it.
    pub feedback_tx: tokio::sync::watch::Sender<u64>,
    /// Bound port of the embedded MCP server, if it started successfully.
    pub mcp_port: Option<u16>,
    keymaps: Keymaps,
    pending: Vec<KeyPress>,
    pending_ticks: u8,
    tick_count: u32,
}

impl App {
    pub fn new(review: Review, loaded: LoadedConfig) -> Self {
        let LoadedConfig {
            config,
            warnings: mut startup_warnings,
            ..
        } = loaded;
        let (theme, theme_warning) = Theme::from_name(&config.ui.theme);
        startup_warnings.extend(theme_warning);
        let mut build = |context| {
            let (keymap, warnings) = Keymap::for_context(context, &config.keys);
            startup_warnings.extend(warnings);
            keymap
        };
        let keymaps = Keymaps {
            status: build(Context::Status),
            diff: build(Context::Diff),
            log: build(Context::Log),
        };

        let mut message = startup_warnings
            .into_iter()
            .next()
            .map(|text| StatusMessage {
                text,
                severity: Severity::Warning,
            });
        let head = match review.vcs.head() {
            Ok(head) => head,
            Err(err) => {
                message = Some(StatusMessage {
                    text: err.to_string(),
                    severity: Severity::Error,
                });
                empty_head()
            }
        };
        let recent = match review.vcs.log(config.ui.recent_commits) {
            Ok(entries) => entries,
            Err(err) => {
                message = Some(StatusMessage {
                    text: err.to_string(),
                    severity: Severity::Error,
                });
                Vec::new()
            }
        };

        Self {
            review,
            head,
            theme,
            config,
            // git config user.name is not exposed through HeadInfo; $USER is
            // a good-enough human label for feedback exports
            author: std::env::var("USER").unwrap_or_else(|_| "you".to_owned()),
            screens: vec![Screen::Status],
            status: StatusView::new(recent),
            log: None,
            diff: None,
            modal: None,
            message,
            pending_osc: None,
            pending_editor: None,
            watcher_healthy: None,
            refresh_flash: 0,
            feedback_tx: tokio::sync::watch::Sender::new(0),
            mcp_port: None,
            keymaps,
            pending: Vec::new(),
            pending_ticks: 0,
            tick_count: 0,
        }
    }

    /// The screen under the cursor; the stack is never empty because `Back`
    /// on the last screen quits instead of popping.
    pub fn screen(&self) -> Screen {
        self.screens.last().copied().unwrap_or(Screen::Status)
    }

    /// Keymap of the active screen, with config remaps applied — what the
    /// hint lines and the help popup render from.
    pub fn active_keymap(&self) -> &Keymap {
        match self.screen().context() {
            Context::Status => &self.keymaps.status,
            Context::Diff => &self.keymaps.diff,
            Context::Log => &self.keymaps.log,
        }
    }

    /// Whether the path carries a current viewed mark, judged against the
    /// review diff (the model viewed hashes are reconciled with).
    pub fn is_path_viewed(&self, path: &str) -> bool {
        self.review
            .model
            .files
            .iter()
            .find(|f| f.path == path)
            .is_some_and(|f| self.review.session.is_viewed(path, &f.content_hash()))
    }

    /// `(files in the review diff, files marked viewed)` for the status bar.
    pub fn viewed_counts(&self) -> (usize, usize) {
        let total = self.review.model.files.len();
        let viewed = self
            .review
            .model
            .files
            .iter()
            .filter(|f| self.review.session.is_viewed(&f.path, &f.content_hash()))
            .count();
        (total, viewed)
    }

    pub fn handle(&mut self, event: AppEvent) -> Flow {
        match event {
            AppEvent::Quit => Flow::Quit,
            AppEvent::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                if self.modal.is_some() {
                    self.handle_modal_key(&key)
                } else {
                    self.handle_key(&key)
                }
            }
            AppEvent::Tick => {
                self.expire_pending();
                self.refresh_flash = self.refresh_flash.saturating_sub(1);
                self.tick_count = self.tick_count.wrapping_add(1);
                if self.tick_count.is_multiple_of(FALLBACK_REFRESH_TICKS)
                    && self.watcher_unhealthy()
                {
                    self.refresh();
                }
                Flow::Continue
            }
            AppEvent::RepoChanged => {
                self.refresh();
                self.refresh_flash = REFRESH_FLASH_TICKS;
                Flow::Continue
            }
            AppEvent::Mcp(request) => {
                // a closed reply channel means the agent already gave up
                // (e.g. timed out while an editor suspended the loop);
                // acting on it would replay a stale mutation unseen
                if request.reply.is_closed() {
                    self.info("dropped stale agent request");
                } else {
                    let response = self.handle_mcp(request.kind);
                    // a dropped receiver means the agent gave up mid-call
                    let _ = request.reply.send(response);
                }
                Flow::Continue
            }
            AppEvent::Key(_) | AppEvent::Mouse(_) | AppEvent::Resize => Flow::Continue,
        }
    }

    fn handle_key(&mut self, key: &KeyEvent) -> Flow {
        // Esc leaves visual selection; it stays out of the keymap because it
        // also drains pending chords and cancels modals everywhere else
        if key.code == KeyCode::Esc && self.visual_active() {
            if let Some(diff) = self.diff.as_mut() {
                diff.visual_anchor = None;
            }
            self.pending.clear();
            return Flow::Continue;
        }
        self.pending_ticks = 0;
        let press = keymap::press_from_event(key);
        let keymap = match self.screen().context() {
            Context::Status => &self.keymaps.status,
            Context::Diff => &self.keymaps.diff,
            Context::Log => &self.keymaps.log,
        };
        let mut pending = std::mem::take(&mut self.pending);
        let resolved = keymap.resolve(&mut pending, press);
        self.pending = pending;
        match resolved {
            Resolved::Action(action) => self.dispatch(action),
            Resolved::Pending | Resolved::Unbound => Flow::Continue,
        }
    }

    fn visual_active(&self) -> bool {
        self.screen() == Screen::Diff
            && self
                .diff
                .as_ref()
                .is_some_and(|d| d.visual_anchor.is_some())
    }

    /// While a modal is up it owns the keyboard.
    fn handle_modal_key(&mut self, key: &KeyEvent) -> Flow {
        match &self.modal {
            Some(Modal::Confirm { .. }) => match key.code {
                KeyCode::Char('y') => self.confirm_modal(),
                KeyCode::Char('n') | KeyCode::Esc => self.modal = None,
                _ => {}
            },
            Some(Modal::Input { .. }) => self.handle_input_key(key),
            Some(Modal::Branch) => self.handle_branch_popup_key(key),
            Some(Modal::BranchList { .. }) => self.handle_branch_list_key(key),
            Some(Modal::Help) => match key.code {
                KeyCode::Esc | KeyCode::Char('q' | '?') => self.modal = None,
                _ => {}
            },
            None => {}
        }
        Flow::Continue
    }

    fn confirm_modal(&mut self) {
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
        }
    }

    fn handle_input_key(&mut self, key: &KeyEvent) {
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
    /// something to be worth persisting.
    fn submit_input(&mut self) {
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
        match on_submit {
            InputOp::Comment { anchor } => {
                self.review.session.add_comment(&self.author, anchor, body);
                if let Some(diff) = self.diff.as_mut() {
                    diff.visual_anchor = None;
                }
                self.after_session_change();
            }
            InputOp::Reply { comment_id } => {
                if self.review.session.reply(&comment_id, &self.author, body) {
                    self.after_session_change();
                } else {
                    self.error("comment is gone; reply dropped");
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
    pub fn feedback_epoch(&self) -> u64 {
        *self.feedback_tx.borrow()
    }

    /// Persist the session and invalidate comment-bearing rows after a
    /// human comment change; also wakes agents waiting for feedback.
    fn after_session_change(&mut self) {
        self.feedback_tx.send_modify(|epoch| *epoch += 1);
        self.after_agent_session_change();
    }

    /// Like [`App::after_session_change`] but for agent-driven mutations,
    /// which must not wake the agent's own `wait_for_feedback` poll.
    pub(crate) fn after_agent_session_change(&mut self) {
        if let Err(err) = self.review.save() {
            self.error(err.to_string());
        }
        if let Some(diff) = self.diff.as_mut() {
            diff.invalidate();
        }
    }

    fn expire_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.pending_ticks += 1;
        if self.pending_ticks >= PENDING_TIMEOUT_TICKS {
            self.pending.clear();
            self.pending_ticks = 0;
        }
    }

    fn dispatch(&mut self, action: Action) -> Flow {
        self.message = None;
        match action {
            Action::Quit => return Flow::Quit,
            Action::Back => return self.pop_screen(),
            Action::Refresh => self.refresh(),
            Action::Help => self.modal = Some(Modal::Help),
            Action::SendFeedback => {
                self.feedback_tx.send_modify(|epoch| *epoch += 1);
                self.info("feedback sent to waiting agents");
            }
            action => match self.screen() {
                Screen::Status => self.dispatch_status(action),
                Screen::Log => self.dispatch_log(action),
                Screen::Diff => self.dispatch_diff(action),
            },
        }
        Flow::Continue
    }

    fn pop_screen(&mut self) -> Flow {
        if self.screens.len() <= 1 {
            return Flow::Quit;
        }
        match self.screens.pop() {
            Some(Screen::Diff) => self.diff = None,
            Some(Screen::Log) => self.log = None,
            Some(Screen::Status) | None => {}
        }
        Flow::Continue
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some(StatusMessage {
            text: text.into(),
            severity: Severity::Info,
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(StatusMessage {
            text: text.into(),
            severity: Severity::Error,
        });
    }

    /// Run a VCS mutation, then refresh so the sections reflect reality.
    pub(crate) fn vcs_op(&mut self, op: impl FnOnce(&dyn Vcs) -> Result<(), VcsError>) {
        match op(self.review.vcs.as_ref()) {
            Ok(()) => self.refresh(),
            Err(err) => self.error(err.to_string()),
        }
    }

    pub(crate) fn refresh(&mut self) {
        let status_anchor = self.status_cursor_anchor();
        let diff_anchor_path = self.diff_cursor_path();
        let fingerprint = self.review.model.fingerprint();
        if let Err(err) = self.review.refresh() {
            self.error(err.to_string());
            return;
        }
        match self.review.vcs.head() {
            Ok(head) => self.head = head,
            Err(err) => self.error(err.to_string()),
        }
        match self.review.vcs.log(self.config.ui.recent_commits) {
            Ok(entries) => self.status.recent = entries,
            Err(err) => self.error(err.to_string()),
        }
        self.restore_status_cursor(status_anchor);
        self.refresh_log();
        if let Some(diff) = self.diff.as_mut() {
            // invalidating drops the visual selection, so a no-op refresh
            // (poll tick, watcher echo) must leave the rows alone
            if self.review.model.fingerprint() != fingerprint {
                diff.invalidate();
            }
            diff.ensure_rows(&self.review);
        }
        self.restore_diff_cursor(diff_anchor_path);
    }

    fn watcher_unhealthy(&self) -> bool {
        self.watcher_healthy
            .as_ref()
            .is_none_or(|healthy| !healthy.load(Ordering::Relaxed))
    }

    // --- branch popup ---

    pub(crate) fn open_branch_popup(&mut self) {
        self.modal = Some(Modal::Branch);
    }

    fn handle_branch_popup_key(&mut self, key: &KeyEvent) {
        match key.code {
            KeyCode::Char('c') => self.branch_name_input(true),
            KeyCode::Char('n') => self.branch_name_input(false),
            KeyCode::Char('b') => self.open_branch_list(BranchAction::Checkout),
            KeyCode::Char('D') => self.open_branch_list(BranchAction::Delete),
            KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
            _ => {}
        }
    }

    fn branch_name_input(&mut self, checkout: bool) {
        let title = if checkout {
            "New branch (checkout)"
        } else {
            "New branch"
        };
        self.modal = Some(Modal::Input {
            title: title.to_owned(),
            buffer: String::new(),
            cursor: 0,
            on_submit: InputOp::CreateBranch { checkout },
        });
    }

    fn open_branch_list(&mut self, action: BranchAction) {
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

    fn handle_branch_list_key(&mut self, key: &KeyEvent) {
        match key.code {
            // back to the branch popup, not all the way out
            KeyCode::Esc | KeyCode::Char('q') => self.modal = Some(Modal::Branch),
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

    fn submit_branch_list(&mut self) {
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

    // --- editor escape ---

    /// Editor command at the point of use: config beats `$DIFFLER_EDITOR`
    /// beats `$EDITOR` beats `vi`.
    fn editor_command(&self) -> String {
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

    /// `cc`: straight to the editor on a gitcommit-style message file when
    /// something is staged.
    pub(crate) fn commit_flow(&mut self) {
        let staged = &self.review.status.staged.files;
        if staged.is_empty() {
            self.info("nothing staged");
            return;
        }
        let template = editor::commit_template(staged);
        // the resolved gitdir, not `<root>/.git`: in a linked worktree the
        // latter is a gitlink file and joining onto it cannot work
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
            purpose: EditorPurpose::Commit { msg_path },
        });
    }

    /// Called by the main loop after the editor subprocess ended and the
    /// terminal is back. `outcome` is the editor's success, or the spawn
    /// failure message.
    pub fn editor_finished(&mut self, purpose: EditorPurpose, outcome: Result<bool, String>) {
        self.message = None;
        match purpose {
            EditorPurpose::Commit { msg_path } => self.finish_commit(&msg_path, outcome),
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

    fn finish_commit(&mut self, msg_path: &Path, outcome: Result<bool, String>) {
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
}

/// Byte offset of the `chars`-th character, for editing the input buffer.
fn byte_index(buffer: &str, chars: usize) -> usize {
    buffer
        .char_indices()
        .nth(chars)
        .map_or(buffer.len(), |(index, _)| index)
}

fn empty_head() -> HeadInfo {
    HeadInfo {
        branch: None,
        oid7: String::new(),
        subject: String::new(),
        upstream: None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::test_support::{Fixture, key, standard_fixture, two_hunk_fixture};

    fn app() -> (Fixture, App) {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        (fixture, app)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle(key(c));
        }
    }

    #[test]
    fn q_quits_from_the_root_screen() {
        let (_fixture, mut app) = app();
        assert_eq!(app.handle(key('q')), Flow::Quit);
    }

    #[test]
    fn back_pops_the_screen_stack_then_quits() {
        let (_fixture, mut app) = app();
        app.handle(key('l'));
        app.handle(key('l'));
        assert_eq!(app.screen(), Screen::Log);
        assert_eq!(app.handle(key('q')), Flow::Continue);
        assert_eq!(app.screens, vec![Screen::Status]);
        assert!(app.log.is_none(), "popping the log screen drops its state");
        assert_eq!(app.handle(key('q')), Flow::Quit);
    }

    #[test]
    fn keymap_follows_the_top_screen() {
        let (_fixture, mut app) = app();
        app.open_working_tree_diff(None);
        // `r` replies in the diff context; on a non-comment row it hints,
        // instead of being swallowed by the status keymap
        app.handle(key('r'));
        let message = app.message.expect("message");
        assert!(message.text.contains("comment"));
    }

    #[test]
    fn head_reflects_the_fixture() {
        let (_fixture, app) = app();
        assert_eq!(app.head.branch.as_deref(), Some("main"));
        assert_eq!(app.head.subject, "initial commit");
        assert_eq!(app.head.oid7.len(), 7);
    }

    #[test]
    fn send_feedback_bumps_the_epoch() {
        let (_fixture, mut app) = app();
        let rx = app.feedback_tx.subscribe();
        app.handle(key('Z'));
        assert_eq!(app.feedback_epoch(), 1);
        assert!(rx.has_changed().unwrap(), "watchers see the bump");
        let message = app.message.expect("message");
        assert!(message.text.contains("feedback"));
    }

    #[test]
    fn human_comment_add_reply_and_resolve_bump_the_epoch() {
        let (_fixture, mut app) = app();
        app.open_working_tree_diff(None);
        // add a comment via the session-backed input modal path
        app.modal = Some(Modal::Input {
            title: "Comment".to_owned(),
            buffer: "why?".to_owned(),
            cursor: 4,
            on_submit: InputOp::Comment {
                anchor: Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    hunk: None,
                    line_text: None,
                },
            },
        });
        app.handle(key('\n'));
        assert_eq!(app.feedback_epoch(), 1, "comment add bumps");

        let id = app.review.session.comments[0].id.clone();
        app.modal = Some(Modal::Input {
            title: "Reply".to_owned(),
            buffer: "because".to_owned(),
            cursor: 7,
            on_submit: InputOp::Reply { comment_id: id },
        });
        app.handle(key('\n'));
        assert_eq!(app.feedback_epoch(), 2, "reply bumps");
    }

    #[test]
    fn stale_mcp_request_is_dropped_without_touching_the_session() {
        let (_fixture, mut app) = app();
        let id = app
            .review
            .session
            .add_comment(
                "human",
                Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    hunk: None,
                    line_text: None,
                },
                "why?",
            )
            .id
            .clone();
        let (reply, rx) = tokio::sync::oneshot::channel();
        // the agent timed out and went away before the app got to the event
        drop(rx);
        let flow = app.handle(AppEvent::Mcp(crate::mcp::McpRequest {
            kind: crate::mcp::McpRequestKind::ReplyComment {
                id,
                body: "late reply".to_owned(),
            },
            reply,
        }));
        assert_eq!(flow, Flow::Continue);
        assert!(
            app.review.session.comments[0].replies.is_empty(),
            "stale mutation must not be replayed"
        );
        let message = app.message.expect("message");
        assert_eq!(message.severity, Severity::Info);
        assert!(message.text.contains("dropped stale agent request"));
    }

    #[test]
    fn live_mcp_request_still_answers_on_the_reply_channel() {
        let (_fixture, mut app) = app();
        let (reply, mut rx) = tokio::sync::oneshot::channel();
        app.handle(AppEvent::Mcp(crate::mcp::McpRequest {
            kind: crate::mcp::McpRequestKind::ReviewStatus,
            reply,
        }));
        assert!(
            matches!(rx.try_recv(), Ok(crate::mcp::McpResponse::Status(_))),
            "live requests are answered"
        );
    }

    #[test]
    fn question_mark_opens_the_help_popup_on_every_screen() {
        let (_fixture, mut app) = app();
        for setup in [
            |_: &mut App| {},
            |app: &mut App| {
                app.handle(key('l'));
                app.handle(key('l'));
            },
            |app: &mut App| app.open_working_tree_diff(None),
        ] {
            setup(&mut app);
            app.handle(key('?'));
            assert_eq!(app.modal, Some(Modal::Help), "{:?}", app.screen());
            // the popup owns the keyboard until dismissed
            app.handle(key('j'));
            assert_eq!(app.modal, Some(Modal::Help));
            app.handle(key('q'));
            assert_eq!(app.modal, None);
        }
    }

    #[test]
    fn help_popup_closes_on_question_mark_and_escape() {
        let (_fixture, mut app) = app();
        app.handle(key('?'));
        app.handle(key('?'));
        assert_eq!(app.modal, None);
        app.handle(key('?'));
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.modal, None);
    }

    #[test]
    fn two_key_commit_chord_starts_the_commit_flow() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        assert_eq!(app.message, None, "first key of a chord stays silent");
        assert_eq!(app.pending_editor, None);
        app.handle(key('c'));
        let request = app.pending_editor.expect("editor request");
        assert!(matches!(request.purpose, EditorPurpose::Commit { .. }));
    }

    #[test]
    fn commit_flow_with_nothing_staged_hints() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('c'));
        app.handle(key('c'));
        assert_eq!(app.pending_editor, None);
        let message = app.message.expect("message");
        assert!(message.text.contains("nothing staged"));
    }

    #[test]
    fn commit_flow_writes_the_template_listing_staged_files() {
        let (fixture, mut app) = app();
        app.handle(key('c'));
        app.handle(key('c'));
        let request = app.pending_editor.clone().expect("editor request");
        let EditorPurpose::Commit { msg_path } = &request.purpose else {
            panic!("expected a commit purpose, got {:?}", request.purpose);
        };
        // the gitdir comes from libgit2, which canonicalizes (macOS tempdirs
        // are symlinked), so compare resolved paths
        assert_eq!(
            msg_path.canonicalize().unwrap(),
            fixture
                .root
                .join(".git/COMMIT_EDITMSG")
                .canonicalize()
                .unwrap()
        );
        let template = std::fs::read_to_string(msg_path).unwrap();
        assert!(template.contains("# Staged:"));
        assert!(template.contains("#\tnew file: ci.yml"));
        // the editor opens the message file itself
        assert_eq!(request.cmd.last().map(String::as_str), msg_path.to_str());
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn commit_flow_writes_the_template_inside_a_linked_worktree_gitdir() {
        let fixture = standard_fixture();
        let wt = fixture.root.parent().unwrap().join("wt");
        git(
            &fixture.root,
            &["worktree", "add", wt.to_str().unwrap(), "-b", "wt-branch"],
        );
        assert!(wt.join(".git").is_file(), ".git is a gitlink file");
        std::fs::write(wt.join("staged.txt"), "in the worktree\n").unwrap();
        git(&wt, &["add", "staged.txt"]);

        let review = Review::open(&wt).expect("review in worktree");
        let mut app = App::new(review, LoadedConfig::default());
        app.handle(key('c'));
        app.handle(key('c'));
        assert_eq!(app.message, None, "template write must succeed");
        let request = app.pending_editor.clone().expect("editor request");
        let EditorPurpose::Commit { msg_path } = &request.purpose else {
            panic!("expected a commit purpose, got {:?}", request.purpose);
        };
        let template = std::fs::read_to_string(msg_path).unwrap();
        assert!(template.contains("#\tnew file: staged.txt"));
        assert!(
            msg_path.components().any(|c| c.as_os_str() == "worktrees"),
            "message file lives in the external gitdir: {}",
            msg_path.display()
        );
    }

    #[test]
    fn editor_finished_commits_the_stripped_message() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        app.handle(key('c'));
        let Some(EditorRequest {
            purpose: EditorPurpose::Commit { msg_path },
            ..
        }) = app.pending_editor.take()
        else {
            panic!("expected a commit request");
        };
        std::fs::write(&msg_path, "add ci config\n\n# comment to strip\n").unwrap();
        app.editor_finished(EditorPurpose::Commit { msg_path }, Ok(true));
        assert_eq!(app.section_files(Section::Staged).len(), 0);
        assert_eq!(app.head.subject, "add ci config");
        let message = app.message.expect("message");
        assert!(message.text.starts_with("committed "), "{}", message.text);
        assert!(message.text.contains(&app.head.oid7));
        assert!(message.text.contains("add ci config"));
    }

    #[test]
    fn an_untouched_template_aborts_the_commit() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        app.handle(key('c'));
        let request = app.pending_editor.take().expect("editor request");
        let head_before = app.head.oid7.clone();
        app.editor_finished(request.purpose, Ok(true));
        let message = app.message.clone().expect("message");
        assert!(message.text.contains("commit aborted"));
        assert_eq!(app.head.oid7, head_before);
        assert_eq!(app.section_files(Section::Staged).len(), 1);
    }

    #[test]
    fn a_failed_editor_aborts_the_commit() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        app.handle(key('c'));
        let request = app.pending_editor.take().expect("editor request");
        app.editor_finished(request.purpose.clone(), Ok(false));
        let message = app.message.clone().expect("message");
        assert!(message.text.contains("commit aborted"));

        app.editor_finished(request.purpose, Err("boom".to_owned()));
        let message = app.message.expect("message");
        assert_eq!(message.severity, Severity::Error);
        assert!(message.text.contains("editor failed"));
        assert!(message.text.contains("boom"));
    }

    #[test]
    fn editor_finished_open_file_refreshes_and_toasts() {
        let (fixture, mut app) = app();
        assert_eq!(app.section_files(Section::Untracked).len(), 1);
        // simulate the editor creating a file while the TUI was suspended
        fixture.write("zzz.md", "new\n");
        app.editor_finished(
            EditorPurpose::OpenFile {
                path: "src/lib.rs".to_owned(),
            },
            Ok(true),
        );
        assert_eq!(app.section_files(Section::Untracked).len(), 2);
        assert_eq!(app.message.expect("message").text, "edited src/lib.rs");
    }

    #[test]
    fn branch_popup_creates_and_checks_out_a_branch() {
        let (_fixture, mut app) = app();
        app.handle(key('b'));
        assert_eq!(app.modal, Some(Modal::Branch));
        app.handle(key('c'));
        assert!(matches!(app.modal, Some(Modal::Input { .. })));
        type_text(&mut app, "feat/x");
        app.handle(key('\n'));
        assert_eq!(app.modal, None);
        assert_eq!(app.head.branch.as_deref(), Some("feat/x"));
        let message = app.message.expect("message");
        assert!(message.text.contains("switched to new branch feat/x"));
    }

    #[test]
    fn branch_popup_n_creates_without_checkout() {
        let (_fixture, mut app) = app();
        app.handle(key('b'));
        app.handle(key('n'));
        type_text(&mut app, "feat/y");
        app.handle(key('\n'));
        assert_eq!(app.head.branch.as_deref(), Some("main"), "HEAD unmoved");
        let branches = app.review.vcs.branches().unwrap();
        assert!(branches.iter().any(|b| b.name == "feat/y" && !b.is_head));
        let message = app.message.expect("message");
        assert!(message.text.contains("created branch feat/y"));
    }

    #[test]
    fn duplicate_branch_name_surfaces_the_error() {
        let (fixture, mut app) = app();
        fixture.branch("feat/dup");
        app.handle(key('b'));
        app.handle(key('n'));
        type_text(&mut app, "feat/dup");
        app.handle(key('\n'));
        let message = app.message.expect("message");
        assert_eq!(message.severity, Severity::Error);
    }

    /// Open the branch list and move the cursor onto `name`.
    fn branch_list_cursor_to(app: &mut App, action_key: char, name: &str) {
        app.handle(key('b'));
        app.handle(key(action_key));
        let Some(Modal::BranchList { branches, .. }) = &app.modal else {
            panic!("expected the branch list, got {:?}", app.modal);
        };
        let target = branches
            .iter()
            .position(|b| b.name == name)
            .expect("branch listed");
        for _ in 0..target {
            app.handle(key('j'));
        }
    }

    #[test]
    fn branch_list_checks_out_the_selected_branch() {
        let (fixture, mut app) = app();
        fixture.branch("feat/topic");
        branch_list_cursor_to(&mut app, 'b', "feat/topic");
        app.handle(key('\n'));
        assert_eq!(app.modal, None);
        assert_eq!(app.head.branch.as_deref(), Some("feat/topic"));
        let message = app.message.expect("message");
        assert!(message.text.contains("checked out feat/topic"));
    }

    #[test]
    fn branch_list_delete_opens_confirm_modal() {
        let (fixture, mut app) = app();
        fixture.branch("feat/dead");
        branch_list_cursor_to(&mut app, 'D', "feat/dead");
        app.handle(key('\n'));
        // Enter should open the confirm modal, not delete immediately
        let Some(Modal::Confirm {
            message,
            on_confirm,
        }) = &app.modal
        else {
            panic!("expected a confirm modal, got {:?}", app.modal);
        };
        assert!(message.contains("feat/dead"));
        assert_eq!(*on_confirm, PendingOp::DeleteBranch("feat/dead".to_owned()));
        // branch still exists before confirming
        let branches = app.review.vcs.branches().unwrap();
        assert!(branches.iter().any(|b| b.name == "feat/dead"));
    }

    #[test]
    fn branch_delete_confirmed_with_y_deletes_the_branch() {
        let (fixture, mut app) = app();
        fixture.branch("feat/dead");
        branch_list_cursor_to(&mut app, 'D', "feat/dead");
        app.handle(key('\n'));
        app.handle(key('y'));
        assert_eq!(app.modal, None);
        let branches = app.review.vcs.branches().unwrap();
        assert!(branches.iter().all(|b| b.name != "feat/dead"));
        let message = app.message.expect("message");
        assert!(message.text.contains("deleted branch feat/dead"));
    }

    #[test]
    fn branch_delete_cancelled_with_n_keeps_the_branch() {
        let (fixture, mut app) = app();
        fixture.branch("feat/dead");
        branch_list_cursor_to(&mut app, 'D', "feat/dead");
        app.handle(key('\n'));
        app.handle(key('n'));
        assert_eq!(app.modal, None);
        let branches = app.review.vcs.branches().unwrap();
        assert!(branches.iter().any(|b| b.name == "feat/dead"));
    }

    #[test]
    fn deleting_the_checked_out_branch_surfaces_the_error() {
        let (_fixture, mut app) = app();
        branch_list_cursor_to(&mut app, 'D', "main");
        app.handle(key('\n'));
        // confirm the deletion attempt
        app.handle(key('y'));
        let message = app.message.expect("message");
        assert_eq!(message.severity, Severity::Error);
        let branches = app.review.vcs.branches().unwrap();
        assert!(branches.iter().any(|b| b.name == "main"));
    }

    #[test]
    fn branch_list_escape_returns_to_the_branch_popup() {
        let (fixture, mut app) = app();
        fixture.branch("feat/topic");
        app.handle(key('b'));
        app.handle(key('b'));
        assert!(matches!(app.modal, Some(Modal::BranchList { .. })));
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.modal, Some(Modal::Branch));
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.modal, None);
    }

    #[test]
    fn repo_changed_refreshes_and_flashes_the_indicator() {
        let (fixture, mut app) = app();
        assert_eq!(app.section_files(Section::Untracked).len(), 1);
        fixture.write("zzz.md", "new\n");
        app.handle(AppEvent::RepoChanged);
        assert_eq!(app.section_files(Section::Untracked).len(), 2);
        assert_eq!(app.refresh_flash, REFRESH_FLASH_TICKS);
        app.handle(AppEvent::Tick);
        assert_eq!(app.refresh_flash, REFRESH_FLASH_TICKS - 1);
    }

    #[test]
    fn tick_fallback_polls_only_while_the_watcher_is_unhealthy() {
        let (fixture, mut app) = app();
        let healthy = Arc::new(AtomicBool::new(true));
        app.watcher_healthy = Some(Arc::clone(&healthy));
        fixture.write("zzz.md", "new\n");
        for _ in 0..FALLBACK_REFRESH_TICKS {
            app.handle(AppEvent::Tick);
        }
        assert_eq!(
            app.section_files(Section::Untracked).len(),
            1,
            "a healthy watcher means no tick polling"
        );
        healthy.store(false, Ordering::Relaxed);
        for _ in 0..FALLBACK_REFRESH_TICKS {
            app.handle(AppEvent::Tick);
        }
        assert_eq!(
            app.section_files(Section::Untracked).len(),
            2,
            "the unhealthy fallback picked up the change"
        );
    }

    #[test]
    fn pending_chord_expires_after_the_timeout() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        for _ in 0..PENDING_TIMEOUT_TICKS {
            app.handle(AppEvent::Tick);
        }
        // the second `c` starts a fresh sequence instead of completing `cc`
        app.handle(key('c'));
        assert_eq!(app.message, None);
    }

    #[test]
    fn unknown_keys_are_a_no_op() {
        let (_fixture, mut app) = app();
        assert_eq!(app.handle(key('z')), Flow::Continue);
        assert_eq!(app.message, None);
        assert_eq!(app.status.cursor, 0);
    }

    #[test]
    fn unknown_theme_surfaces_a_warning() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.theme = "nope".to_owned();
        let app = App::new(fixture.review(), loaded);
        let message = app.message.expect("warning");
        assert_eq!(message.severity, Severity::Warning);
        assert!(message.text.contains("nope"));
    }

    #[test]
    fn config_key_override_reaches_the_keymap() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded
            .config
            .keys
            .status
            .insert("move_down".to_owned(), "n".to_owned());
        let mut app = App::new(fixture.review(), loaded);
        app.handle(key('n'));
        assert_eq!(app.status.cursor, 1);
    }

    #[test]
    fn input_modal_edits_the_buffer() {
        let (_fixture, mut app) = app();
        app.modal = Some(Modal::Input {
            title: "Test".to_owned(),
            buffer: String::new(),
            cursor: 0,
            on_submit: InputOp::Reply {
                comment_id: "missing".to_owned(),
            },
        });
        for c in "héllo".chars() {
            app.handle(key(c));
        }
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::NONE,
        )));
        let Some(Modal::Input { buffer, cursor, .. }) = &app.modal else {
            panic!("modal should still be up");
        };
        assert_eq!(buffer, "héll");
        assert_eq!(*cursor, 4);
        // Esc cancels without touching the session
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.modal, None);
        assert!(app.review.session.comments.is_empty());
    }

    #[test]
    fn alt_enter_inserts_a_newline_and_the_body_keeps_both_lines() {
        let (_fixture, mut app) = app();
        app.modal = Some(Modal::Input {
            title: "Comment".to_owned(),
            buffer: String::new(),
            cursor: 0,
            on_submit: InputOp::Comment {
                anchor: Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    hunk: None,
                    line_text: None,
                },
            },
        });
        for c in "first".chars() {
            app.handle(key(c));
        }
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::ALT,
        )));
        for c in "second".chars() {
            app.handle(key(c));
        }
        let Some(Modal::Input { buffer, cursor, .. }) = &app.modal else {
            panic!("modal should still be up");
        };
        assert_eq!(buffer, "first\nsecond");
        assert_eq!(*cursor, 12);
        app.handle(key('\n'));
        assert_eq!(app.modal, None);
        assert_eq!(app.review.session.comments[0].body, "first\nsecond");
    }

    #[test]
    fn ctrl_j_is_a_newline_fallback() {
        let (_fixture, mut app) = app();
        app.modal = Some(Modal::Input {
            title: "Test".to_owned(),
            buffer: "ab".to_owned(),
            cursor: 1,
            on_submit: InputOp::Reply {
                comment_id: "missing".to_owned(),
            },
        });
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
        )));
        let Some(Modal::Input { buffer, cursor, .. }) = &app.modal else {
            panic!("modal should still be up");
        };
        assert_eq!(buffer, "a\nb");
        assert_eq!(*cursor, 2);
    }

    #[test]
    fn empty_input_submit_is_a_cancel() {
        let (_fixture, mut app) = app();
        app.modal = Some(Modal::Input {
            title: "Test".to_owned(),
            buffer: "   ".to_owned(),
            cursor: 3,
            on_submit: InputOp::Reply {
                comment_id: "missing".to_owned(),
            },
        });
        app.handle(key('\n'));
        assert_eq!(app.modal, None);
        assert_eq!(app.message, None, "no error: empty submit just closes");
    }
}
