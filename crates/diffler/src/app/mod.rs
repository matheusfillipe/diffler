//! Application state and event handling. `App::handle` is a pure-ish state
//! transition (no terminal IO) so the whole shell is unit-testable; rendering
//! reads the state in `ui::draw`. Per-screen state and handlers live in the
//! `status`, `log`, and `diff` submodules.

mod diff;
mod log;
pub mod logs;
mod mcp;
mod status;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) use diff::build_split_rows;
pub use diff::{
    CommentLine, DiffRow, DiffSource, DiffView, FileHighlights, FileScope, Pane, SplitRow,
    comment_display,
};
pub use log::LogView;
pub(crate) use status::{CI_TITLE, RECENT_TITLE};
pub use status::{Row, Section, StatusView};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use diffler_core::review::Review;
use diffler_core::session::Anchor;
use diffler_core::vcs::{BranchInfo, HeadInfo, NetworkOp, Vcs, VcsError};

use crate::config::{Config, KeyPress, LoadedConfig};
use crate::editor::{self, EditorPurpose, EditorRequest};
use crate::event::AppEvent;
use crate::keymap::{self, Action, Context, Keymap, Resolved};
use crate::search::Search;
use crate::theme::Theme;
use crate::transient::{Transient, TransientKind, TransientResolve};

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
    /// The CI runs start page (list of recent runs for the repo's provider).
    Runs,
    /// The CI graph view; keys route to the embedded `GraphView`, not the keymap.
    Graph,
    /// A single job's log view.
    Logs,
}

impl Screen {
    fn context(self) -> Context {
        match self {
            // the Runs/Graph screens drive their own input, so their keymap
            // context is unused; map them to Status so hint/help lookups stay total.
            Self::Status | Self::Runs | Self::Graph => Context::Status,
            Self::Logs => Context::Logs,
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
    /// Branch picker feeding `action` with the selected name.
    BranchList {
        branches: Vec<BranchInfo>,
        cursor: usize,
        action: BranchAction,
    },
    /// Keymap listing for the screen the popup opened over.
    Help,
}

/// A network git op the main loop runs by shelling out, with the terminal kept
/// up: it spawns a blocking task so the event loop keeps drawing, and the
/// result returns as [`AppEvent::GitDone`]. Set by a push/pull/fetch leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOp {
    /// Human label for the status bar, e.g. "push".
    pub label: String,
    /// Full argv: program followed by its arguments.
    pub argv: Vec<String>,
}

struct Keymaps {
    status: Keymap,
    diff: Keymap,
    log: Keymap,
    logs: Keymap,
}

impl Keymaps {
    /// Build the per-context keymaps, draining any binding warnings into `sink`.
    fn build(keys: &crate::config::KeysConfig, sink: &mut Vec<String>) -> Self {
        let mut build = |context| {
            let (keymap, warnings) = Keymap::for_context(context, keys);
            sink.extend(warnings);
            keymap
        };
        Self {
            status: build(Context::Status),
            diff: build(Context::Diff),
            log: build(Context::Log),
            logs: build(Context::Logs),
        }
    }
}

/// Built transients, applied with config overrides once at startup.
struct Transients {
    commit: Transient,
    branch: Transient,
    log: Transient,
    push: Transient,
    pull: Transient,
    fetch: Transient,
    stash: Transient,
}

impl Transients {
    fn get(&self, kind: TransientKind) -> &Transient {
        match kind {
            TransientKind::Commit => &self.commit,
            TransientKind::Branch => &self.branch,
            TransientKind::Log => &self.log,
            TransientKind::Push => &self.push,
            TransientKind::Pull => &self.pull,
            TransientKind::Fetch => &self.fetch,
            TransientKind::Stash => &self.stash,
        }
    }
}

/// An open transient awaiting its next key. `opened_at` is a tick count so the
/// which-key reveal timer never reads a wall clock in render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenTransient {
    pub kind: TransientKind,
    opened_at: u32,
}

/// Ticks (250ms each) the transient stays armed before the which-key panel is
/// revealed, so a fast resolving key never flashes the panel.
const WHICH_KEY_REVEAL_TICKS: u32 = 1;

/// Pending multi-key sequences die after this many 250ms ticks.
const PENDING_TIMEOUT_TICKS: u8 = 4;
/// How long the post-refresh `↻` status-bar indicator stays up.
const REFRESH_FLASH_TICKS: u8 = 4;
/// Poll interval (in 250ms ticks) when the watcher is missing or broken.
const FALLBACK_REFRESH_TICKS: u32 = 20;

/// What the main loop should fetch from the CI provider off-thread. Mirrors
/// `GitOp`/`pending_git`: set by the app, taken once by the loop, result
/// returned as an `AppEvent`.
#[derive(Debug, Clone)]
pub enum CiRequest {
    Runs,
    Detail(diffler_ci::RunId),
    Log {
        run: diffler_ci::RunId,
        job: diffler_ci::JobId,
        offset: u64,
    },
}

/// Detect the repo's CI provider from its `origin` remote (via the `Vcs` trait,
/// not a subprocess) and config-file presence.
fn detect_ci(review: &Review, ci: &crate::config::CiConfig) -> Option<diffler_ci::Detected> {
    let remote = review.vcs.remote_url("origin").ok().flatten();
    crate::ci::detect_for_repo(&review.repo_root, remote.as_deref(), ci)
}

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
    /// The embedded CI graph component, present while the Graph screen is up.
    pub graph: Option<diffler_graph::GraphView>,
    /// Detected CI provider for the repo, computed at startup. `None` when no
    /// provider could be determined (no recognized remote or config file).
    ci_detected: Option<diffler_ci::Detected>,
    /// Recent CI runs shown on the Runs screen.
    pub runs: Vec<diffler_ci::CiRun>,
    runs_cursor: usize,
    /// The run opened into the graph, re-polled for live status.
    open_run: Option<diffler_ci::RunId>,
    /// The job whose log is on the Logs screen.
    open_job: Option<diffler_ci::JobId>,
    /// Accumulated raw job-log text and the byte offset the next poll resumes
    /// from. Parsed into [`logs`](Self::logs) for the foldable view.
    log_text: String,
    log_offset: u64,
    /// The foldable step view over `log_text`, present while the Logs screen is up.
    pub logs: Option<logs::LogsView>,
    /// Set once a log chunk reports the job's log is complete, so polling stops
    /// (a dump-mode provider returns the whole log in one chunk).
    log_done: bool,
    /// A CI provider call the main loop should run off-thread (mirrors `pending_git`).
    pub pending_ci: Option<CiRequest>,
    pub modal: Option<Modal>,
    /// Active `/` search over the focused pane, if any. `search.open` means the
    /// prompt is capturing input; otherwise highlights persist while `n`/`N`
    /// navigate.
    pub search: Option<Search>,
    pub message: Option<StatusMessage>,
    /// Text to put on the system clipboard. The main loop, after the next
    /// draw, emits it as an OSC52 sequence (covers ssh/tmux) and also pipes it
    /// to the platform clipboard tool, then clears.
    pub pending_clipboard: Option<String>,
    /// Editor subprocess the main loop runs with the terminal suspended,
    /// then reports back through [`App::editor_finished`].
    pub pending_editor: Option<EditorRequest>,
    /// Network git op the main loop runs in the background (terminal stays up),
    /// reporting back through [`AppEvent::GitDone`]. Set by a push/pull/fetch
    /// transient leaf, taken once by the loop.
    pub pending_git: Option<GitOp>,
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
    transients: Transients,
    /// The open transient, if any. Set when a top-level prefix fires; cleared
    /// when a key resolves, Esc aborts, or an unknown key closes it.
    pub transient: Option<OpenTransient>,
    pending: Vec<KeyPress>,
    pending_ticks: u8,
    tick_count: u32,
    /// Time and cell of the last left-press, for double-click detection.
    last_click: Option<(std::time::Instant, u16, u16)>,
    /// Wall-clock seconds, refreshed with the commit list, for rendering
    /// commit ages ("3h ago"). A field so tests can pin it.
    pub now_unix: i64,
}

/// Current wall-clock time in unix seconds, or 0 if the clock is before the
/// epoch (it never is).
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
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
        crate::ui::diff::init_highlighter(theme.syntax);
        let keymaps = Keymaps::build(&config.keys, &mut startup_warnings);
        let mut build_transient = |kind| {
            let (transient, warnings) = Transient::build(kind, &config.keys);
            startup_warnings.extend(warnings);
            transient
        };
        let transients = Transients {
            commit: build_transient(TransientKind::Commit),
            branch: build_transient(TransientKind::Branch),
            log: build_transient(TransientKind::Log),
            push: build_transient(TransientKind::Push),
            pull: build_transient(TransientKind::Pull),
            fetch: build_transient(TransientKind::Fetch),
            stash: build_transient(TransientKind::Stash),
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

        let ci_detected = detect_ci(&review, &config.ci);

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
            graph: None,
            // kick an initial CI fetch so the Status section populates at launch
            // (evaluated before `ci_detected` is moved into the struct below)
            pending_ci: ci_detected.is_some().then_some(CiRequest::Runs),
            ci_detected,
            runs: Vec::new(),
            runs_cursor: 0,
            open_run: None,
            open_job: None,
            log_text: String::new(),
            log_offset: 0,
            logs: None,
            log_done: false,
            modal: None,
            search: None,
            message,
            pending_clipboard: None,
            pending_editor: None,
            pending_git: None,
            watcher_healthy: None,
            refresh_flash: 0,
            feedback_tx: tokio::sync::watch::Sender::new(0),
            mcp_port: None,
            keymaps,
            transients,
            transient: None,
            pending: Vec::new(),
            pending_ticks: 0,
            tick_count: 0,
            last_click: None,
            now_unix: now_unix(),
        }
    }

    /// The screen under the cursor; the stack is never empty because `Back`
    /// on the last screen quits instead of popping.
    pub fn screen(&self) -> Screen {
        self.screens.last().copied().unwrap_or(Screen::Status)
    }

    /// Index of the selected run on the Runs screen.
    pub fn runs_selected(&self) -> usize {
        self.runs_cursor
    }

    /// The accumulated job-log text on the Logs screen.
    pub fn log_text(&self) -> &str {
        &self.log_text
    }

    /// The foldable step view over the Logs screen, once a log chunk arrived.
    pub fn logs(&self) -> Option<&logs::LogsView> {
        self.logs.as_ref()
    }

    /// The detected CI provider for the repo, if any (the main loop builds a
    /// provider from this to service a `pending_ci` request).
    pub fn ci_detected(&self) -> Option<diffler_ci::Detected> {
        self.ci_detected.clone()
    }

    /// Keymap of the active screen, with config remaps applied — what the
    /// hint lines and the help popup render from.
    pub fn active_keymap(&self) -> &Keymap {
        match self.screen().context() {
            Context::Status => &self.keymaps.status,
            Context::Diff => &self.keymaps.diff,
            Context::Log => &self.keymaps.log,
            Context::Logs => &self.keymaps.logs,
        }
    }

    /// Whether the path carries a current viewed mark, judged against the
    /// review diff (the model viewed hashes are reconciled with).
    pub fn is_path_viewed(&self, path: &str) -> bool {
        self.review
            .model()
            .files
            .iter()
            .find(|f| f.path == path)
            .is_some_and(|f| self.review.session.is_viewed(path, &f.content_hash()))
    }

    /// `(files in the review diff, files marked viewed)` for the status bar.
    pub fn viewed_counts(&self) -> (usize, usize) {
        let model = self.review.model();
        let total = model.files.len();
        let viewed = model
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
                if self.screen() == Screen::Graph {
                    self.handle_graph_key(&key)
                } else if self.screen() == Screen::Runs {
                    self.handle_runs_key(&key)
                } else if self.modal.is_some() {
                    self.handle_modal_key(&key)
                } else if self.transient.is_some() {
                    self.handle_transient_key(&key)
                } else if self.search.as_ref().is_some_and(|s| s.open) {
                    self.handle_search_key(&key)
                } else if key.code == KeyCode::Esc && self.search.is_some() {
                    self.search = None;
                    Flow::Continue
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
                // re-poll the active CI screen on a relaxed cadence (250ms ticks);
                // saturating + clamp so a pathological config can't zero or overflow it
                let poll_ticks =
                    u32::try_from(self.config.ci.poll_seconds.max(1).saturating_mul(4))
                        .unwrap_or(u32::MAX);
                if self.tick_count.is_multiple_of(poll_ticks) {
                    self.queue_ci_poll();
                }
                Flow::Continue
            }
            AppEvent::CiRuns(runs) => {
                self.runs = runs;
                self.runs_cursor = self.runs_cursor.min(self.runs.len().saturating_sub(1));
                // the inline Status section grew/shrank; keep the row cursor valid
                self.clamp_cursor();
                Flow::Continue
            }
            AppEvent::CiRunDetail(detail) => {
                let model = crate::ci::to_model(&detail);
                if let Some(graph) = self.graph.as_mut() {
                    graph.set_model(model);
                }
                Flow::Continue
            }
            AppEvent::CiLog {
                text,
                next_offset,
                done,
            } => {
                self.log_text.push_str(&text);
                self.log_offset = next_offset;
                self.log_done = done;
                let rebuilt = logs::LogsView::parse(&self.log_text);
                self.logs = Some(match self.logs.take() {
                    Some(prev) => prev.carry_into(rebuilt),
                    None => rebuilt,
                });
                Flow::Continue
            }
            AppEvent::CiError(message) => {
                self.error(message);
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
            AppEvent::GitDone { label, ok, output } => {
                self.git_finished(&label, ok, &output);
                Flow::Continue
            }
            // mouse only drives the plain screens; a modal or transient owns
            // input while open
            AppEvent::Mouse(mouse) if self.modal.is_none() && self.transient.is_none() => {
                self.handle_mouse(mouse);
                Flow::Continue
            }
            AppEvent::Key(_) | AppEvent::Mouse(_) | AppEvent::Resize => Flow::Continue,
        }
    }

    /// Translate a raw mouse event into a [`MouseGesture`] and dispatch it to
    /// the active screen. Each screen implements `*_mouse(MouseGesture)` and
    /// must handle every variant — so the `match self.screen()` here is the one
    /// place that forces a new screen to wire up mouse support (it won't
    /// compile without an arm), and the exhaustive gesture match in each
    /// handler forces every interaction to be considered.
    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        // the graph view consumes raw mouse events itself
        if self.screen() == Screen::Graph {
            if let Some(action) = self.graph.as_mut().and_then(|g| g.on_mouse(mouse)) {
                self.on_graph_action(&action);
            }
            return;
        }
        let (col, row) = (mouse.column, mouse.row);
        let gesture = match mouse.kind {
            MouseEventKind::ScrollDown => MouseGesture::Scroll {
                col,
                row,
                down: true,
            },
            MouseEventKind::ScrollUp => MouseGesture::Scroll {
                col,
                row,
                down: false,
            },
            MouseEventKind::Down(MouseButton::Left) => {
                if self.is_double_click(col, row) {
                    MouseGesture::DoublePress { col, row }
                } else {
                    MouseGesture::Press { col, row }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => MouseGesture::Drag { col, row },
            MouseEventKind::Down(MouseButton::Right) => MouseGesture::Cancel,
            _ => return,
        };
        match self.screen() {
            Screen::Status => self.status_mouse(gesture),
            Screen::Diff => self.diff_mouse(gesture),
            Screen::Log => self.log_mouse(gesture),
            // the CI screens are keyboard-driven; Graph consumes mouse above
            Screen::Graph | Screen::Runs | Screen::Logs => {}
        }
    }

    /// A second left-press at (about) the same cell within the double-click
    /// window. Resets after firing so a third press starts fresh.
    fn is_double_click(&mut self, col: u16, row: u16) -> bool {
        let now = std::time::Instant::now();
        let double = self.last_click.is_some_and(|(at, c, r)| {
            now.duration_since(at) < DOUBLE_CLICK_WINDOW && c.abs_diff(col) <= 1 && r == row
        });
        self.last_click = if double { None } else { Some((now, col, row)) };
        double
    }

    fn handle_key(&mut self, key: &KeyEvent) -> Flow {
        // Esc leaves visual selection; it stays out of the keymap because it
        // also drains pending chords and cancels modals everywhere else
        if key.code == KeyCode::Esc && self.visual_active() {
            match self.screen() {
                Screen::Diff => {
                    if let Some(diff) = self.diff.as_mut() {
                        diff.visual_anchor = None;
                    }
                }
                Screen::Log => {
                    if let Some(log) = self.log.as_mut() {
                        log.visual_anchor = None;
                    }
                }
                Screen::Logs => {
                    if let Some(view) = self.logs.as_mut() {
                        view.visual_anchor = None;
                    }
                }
                Screen::Status | Screen::Graph | Screen::Runs => {}
            }
            self.pending.clear();
            return Flow::Continue;
        }
        self.pending_ticks = 0;
        let press = keymap::press_from_event(key);
        let mut pending = std::mem::take(&mut self.pending);
        let resolved = self.active_keymap().resolve(&mut pending, press);
        self.pending = pending;
        match resolved {
            Resolved::Action(action) => self.dispatch(action),
            Resolved::Transient(kind) => {
                self.open_transient(kind);
                Flow::Continue
            }
            Resolved::Pending | Resolved::Unbound => Flow::Continue,
        }
    }

    fn open_transient(&mut self, kind: TransientKind) {
        self.message = None;
        self.transient = Some(OpenTransient {
            kind,
            opened_at: self.tick_count,
        });
    }

    /// While a transient is armed it owns the keyboard: Esc/Backspace close it,
    /// a leaf key fires and closes, an unknown key closes with a beep
    /// (neogit-style).
    fn handle_transient_key(&mut self, key: &KeyEvent) -> Flow {
        let Some(open) = self.transient else {
            return Flow::Continue;
        };
        // a single-level transient has nothing to pop, so Backspace and Esc
        // both abort it without dispatching
        if matches!(key.code, KeyCode::Esc | KeyCode::Backspace) {
            self.transient = None;
            return Flow::Continue;
        }
        let press = keymap::press_from_event(key);
        let resolved = self.transients.get(open.kind).resolve(&press);
        self.transient = None;
        match resolved {
            TransientResolve::Action(action) => self.dispatch(action),
            TransientResolve::Unbound => {
                // neogit beeps and closes on an unbound key in a transient
                self.info("no such command");
                Flow::Continue
            }
        }
    }

    /// The built transient for `kind`, with config overrides applied — what
    /// the help popup reads.
    pub fn transient(&self, kind: TransientKind) -> &Transient {
        self.transients.get(kind)
    }

    /// The transient panel to reveal: `Some` once the reveal timer has elapsed
    /// since the transient opened, so a fast resolving key never flashes it.
    pub fn which_key_panel(&self) -> Option<&Transient> {
        let open = self.transient?;
        if self.tick_count.wrapping_sub(open.opened_at) >= WHICH_KEY_REVEAL_TICKS {
            Some(self.transients.get(open.kind))
        } else {
            None
        }
    }

    fn visual_active(&self) -> bool {
        match self.screen() {
            Screen::Diff => self
                .diff
                .as_ref()
                .is_some_and(|d| d.visual_anchor.is_some()),
            Screen::Log => self.log.as_ref().is_some_and(|l| l.visual_anchor.is_some()),
            Screen::Logs => self
                .logs
                .as_ref()
                .is_some_and(|v| v.visual_anchor.is_some()),
            Screen::Status | Screen::Graph | Screen::Runs => false,
        }
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

    /// The review source the user is currently looking at: the open diff's
    /// source, or the working tree on the status screen.
    pub(crate) fn active_review_source(&self) -> DiffSource {
        self.diff
            .as_ref()
            .map_or(DiffSource::WorkingTree, |diff| diff.source.clone())
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
        let source = self.active_review_source();
        if let Err(err) = self.review.save_for(&source) {
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
            Action::Search => self.search_start(),
            Action::SearchNext => self.search_step(true),
            Action::SearchPrev => self.search_step(false),
            Action::OpenRuns => self.open_runs(),
            action => match self.screen() {
                Screen::Status => self.dispatch_status(action),
                Screen::Log => self.dispatch_log(action),
                Screen::Diff => self.dispatch_diff(action),
                Screen::Logs => self.dispatch_logs(action),
                // the Runs/Graph screens drive their own input, never the keymap
                Screen::Graph | Screen::Runs => {}
            },
        }
        Flow::Continue
    }

    /// Open the CI runs start page for the repo's detected provider.
    fn open_runs(&mut self) {
        if self.ci_detected.is_none() {
            self.info("no CI provider detected for this repo");
            return;
        }
        self.runs_cursor = 0;
        self.push_screen(Screen::Runs);
        self.pending_ci = Some(CiRequest::Runs);
    }

    /// Open the selected run's graph: fetch its detail, which arrives as
    /// `AppEvent::CiRunDetail` and feeds the graph view.
    fn open_selected_run(&mut self) {
        let Some(run) = self.runs.get(self.runs_cursor) else {
            return;
        };
        let id = run.id.clone();
        self.open_run = Some(id.clone());
        self.graph = Some(diffler_graph::GraphView::new());
        self.push_screen(Screen::Graph);
        self.pending_ci = Some(CiRequest::Detail(id));
    }

    /// Open a job's log view from a graph node activation.
    fn open_logs(&mut self, job: diffler_ci::JobId) {
        let Some(run) = self.open_run.clone() else {
            return;
        };
        self.open_job = Some(job.clone());
        self.log_text.clear();
        self.log_offset = 0;
        self.logs = None;
        self.log_done = false;
        self.push_screen(Screen::Logs);
        self.pending_ci = Some(CiRequest::Log {
            run,
            job,
            offset: 0,
        });
    }

    /// Queue the poll for the active CI screen onto `pending_ci`.
    fn queue_ci_poll(&mut self) {
        self.pending_ci = match self.screen() {
            // the Status screen shows an inline CI-runs section, kept live
            Screen::Status | Screen::Runs => Some(CiRequest::Runs),
            Screen::Graph => self.open_run.clone().map(CiRequest::Detail),
            // stop once the log is complete (a dump provider sends it all at once)
            Screen::Logs if self.log_done => None,
            Screen::Logs => match (self.open_run.clone(), self.open_job.clone()) {
                (Some(run), Some(job)) => Some(CiRequest::Log {
                    run,
                    job,
                    offset: self.log_offset,
                }),
                _ => None,
            },
            _ => None,
        };
    }

    /// While the runs screen is up: navigate the list, Enter opens a run.
    fn handle_runs_key(&mut self, key: &KeyEvent) -> Flow {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return self.pop_screen(),
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.runs.is_empty() {
                    self.runs_cursor = (self.runs_cursor + 1).min(self.runs.len() - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.runs_cursor = self.runs_cursor.saturating_sub(1);
            }
            KeyCode::Enter => self.open_selected_run(),
            _ => {}
        }
        Flow::Continue
    }

    /// Drive the foldable logs view from a keymap [`Action`]: motions, fold,
    /// visual select, and yank. The Logs screen reuses the diff/log keymap.
    fn dispatch_logs(&mut self, action: Action) {
        let Some(view) = self.logs.as_mut() else {
            return;
        };
        let last = view.rows().len().saturating_sub(1);
        match action {
            Action::MoveDown => view.cursor = (view.cursor + 1).min(last),
            Action::MoveUp => view.cursor = view.cursor.saturating_sub(1),
            Action::GoTop => view.cursor = 0,
            Action::GoBottom => view.cursor = last,
            Action::HalfPageDown => self.logs_page(false, false),
            Action::HalfPageUp => self.logs_page(true, false),
            Action::FullPageDown => self.logs_page(false, true),
            Action::FullPageUp => self.logs_page(true, true),
            Action::ToggleFold => view.toggle_fold_at_cursor(),
            Action::VisualSelect => {
                view.visual_anchor = match view.visual_anchor {
                    Some(_) => None,
                    None => Some(view.cursor),
                };
            }
            Action::CopyFileFeedback | Action::CopyAllFeedback => {
                self.pending_clipboard = Some(view.selection_text());
                let view = self.logs.as_mut();
                if let Some(view) = view {
                    view.visual_anchor = None;
                }
                self.info("yanked log selection");
            }
            _ => {}
        }
    }

    /// Half/full-page cursor jump over the logs view, mirroring `log_page`.
    fn logs_page(&mut self, up: bool, full: bool) {
        let Some(view) = self.logs.as_mut() else {
            return;
        };
        let last = view.rows().len().saturating_sub(1);
        let page = usize::from(view.viewport).max(1);
        let step = if full { page } else { (page / 2).max(1) };
        view.cursor = if up {
            view.cursor.saturating_sub(step)
        } else {
            (view.cursor + step).min(last)
        };
    }

    /// While the graph screen is up, keys go to the component; Esc/q leave it.
    fn handle_graph_key(&mut self, key: &KeyEvent) -> Flow {
        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
            return self.pop_screen();
        }
        if let Some(action) = self.graph.as_mut().and_then(|g| g.on_key(*key)) {
            self.on_graph_action(&action);
        }
        Flow::Continue
    }

    /// React to a [`diffler_graph::GraphAction`] from the component: activating a
    /// node opens that job's log.
    fn on_graph_action(&mut self, action: &diffler_graph::GraphAction) {
        match action {
            diffler_graph::GraphAction::Activated(id) => {
                self.open_logs(diffler_ci::JobId(id.0.clone()));
            }
            diffler_graph::GraphAction::Folded { .. } => {}
        }
    }

    /// Enter a screen. Clears any search, whose matches are keyed to the
    /// leaving screen's rows.
    fn push_screen(&mut self, screen: Screen) {
        self.search = None;
        self.screens.push(screen);
    }

    fn pop_screen(&mut self) -> Flow {
        if self.screens.len() <= 1 {
            return Flow::Quit;
        }
        self.search = None;
        match self.screens.pop() {
            Some(Screen::Diff) => self.diff = None,
            Some(Screen::Log) => self.log = None,
            Some(Screen::Graph) => {
                self.graph = None;
                self.open_run = None;
            }
            Some(Screen::Logs) => {
                self.open_job = None;
                self.log_text.clear();
                self.logs = None;
            }
            Some(Screen::Runs) => self.runs.clear(),
            Some(Screen::Status) | None => {}
        }
        Flow::Continue
    }

    fn search_start(&mut self) {
        let origin = self.focused_cursor_row();
        let rows = self.focused_search_rows();
        let mut search = Search::start(origin);
        search.recompute(&rows);
        self.search = Some(search);
    }

    fn handle_search_key(&mut self, key: &KeyEvent) -> Flow {
        match key.code {
            KeyCode::Esc => self.search_cancel(),
            KeyCode::Enter => self.search_commit(),
            KeyCode::Backspace => self.search_edit(Search::backspace),
            KeyCode::Char(c) => self.search_edit(|s| s.insert(c)),
            _ => {}
        }
        Flow::Continue
    }

    fn search_edit(&mut self, edit: impl FnOnce(&mut Search)) {
        if let Some(s) = self.search.as_mut() {
            edit(s);
        }
        let rows = self.focused_search_rows();
        if let Some(s) = self.search.as_mut() {
            s.recompute(&rows);
        }
        if let Some(row) = self.search.as_ref().and_then(Search::current_row) {
            self.focus_searched_row(row);
        }
    }

    fn search_step(&mut self, forward: bool) {
        let row = match self.search.as_mut() {
            Some(s) if !s.open => {
                if forward {
                    s.next_match()
                } else {
                    s.prev_match()
                }
            }
            _ => return,
        };
        if let Some(row) = row {
            self.focus_searched_row(row);
        }
    }

    fn search_commit(&mut self) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        if search.query().is_empty() {
            self.search = None;
            return;
        }
        if let Some(row) = search.commit() {
            self.focus_searched_row(row);
        }
    }

    fn search_cancel(&mut self) {
        if let Some(origin) = self.search.take().map(|s| s.origin_row()) {
            self.focus_searched_row(origin);
        }
    }

    fn focused_cursor_row(&self) -> usize {
        match self.screen() {
            Screen::Status => self.status.cursor,
            Screen::Log => self.log.as_ref().map_or(0, |l| l.cursor),
            Screen::Diff => self.diff.as_ref().map_or(0, |d| match d.focus {
                Pane::List => d.tree_cursor,
                Pane::Diff => d.cursor,
            }),
            Screen::Logs => self.logs.as_ref().map_or(0, |v| v.cursor),
            Screen::Graph | Screen::Runs => 0,
        }
    }

    fn focused_search_rows(&self) -> Vec<(usize, String)> {
        match self.screen() {
            Screen::Status => self.status_search_rows(),
            Screen::Log => self.log.as_ref().map_or_else(Vec::new, |log| {
                log.entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| (i, e.subject.clone()))
                    .collect()
            }),
            Screen::Diff => self.diff_search_rows(),
            Screen::Logs => self.logs.as_ref().map_or_else(Vec::new, |view| {
                view.rows()
                    .iter()
                    .enumerate()
                    .map(|(i, row)| (i, view.row_text(*row).to_owned()))
                    .collect()
            }),
            Screen::Graph | Screen::Runs => Vec::new(),
        }
    }

    fn diff_search_rows(&self) -> Vec<(usize, String)> {
        let Some(diff) = self.diff.as_ref() else {
            return Vec::new();
        };
        let model = diff.model(&self.review);
        match diff.focus {
            Pane::List => diff
                .tree_rows(model)
                .iter()
                .enumerate()
                .map(|(i, r)| (i, tree_row_label(&r.node)))
                .collect(),
            Pane::Diff => {
                let file = model.files.get(diff.selected);
                diff.rows()
                    .iter()
                    .enumerate()
                    .filter_map(|(i, row)| diff_row_text(file, row).map(|t| (i, t)))
                    .collect()
            }
        }
    }

    fn focus_searched_row(&mut self, row: usize) {
        match self.screen() {
            Screen::Status => self.status.cursor = row,
            Screen::Log => {
                if let Some(l) = self.log.as_mut() {
                    l.cursor = row;
                }
            }
            Screen::Diff => {
                if let Some(d) = self.diff.as_mut() {
                    match d.focus {
                        Pane::List => d.tree_cursor = row,
                        Pane::Diff => d.cursor = row,
                    }
                }
            }
            Screen::Logs => {
                if let Some(v) = self.logs.as_mut() {
                    v.cursor = row;
                }
            }
            Screen::Graph | Screen::Runs => {}
        }
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
        self.now_unix = now_unix();
        let status_anchor = self.status_cursor_anchor();
        let diff_anchor_path = self.diff_cursor_path();
        let fingerprint = self.review.model().fingerprint();
        if let Err(err) = self.review.refresh() {
            self.error(err.to_string());
            return;
        }
        // rebuilt models carry no emphasis; reset the per-file enrich memos
        self.status.clear_enriched();
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
            // a no-op refresh keeps the rows (and visual selection) but still
            // rebuilt the model unenriched, so the emphasis memo must reset
            diff.clear_enriched();
            // invalidating drops the visual selection, so a no-op refresh
            // (poll tick, watcher echo) must leave the rows alone
            if self.review.model().fingerprint() != fingerprint {
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

    // --- branch transient flows ---

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

    // --- network ops (push/pull/fetch) ---

    /// Queue a network git op: resolve its argv from the backend and set
    /// `pending_git`, plus a "running …" status so the next draw shows it. The
    /// main loop runs the process in the background and reports back through
    /// [`AppEvent::GitDone`], so the event loop never freezes on the network.
    pub(crate) fn request_network(&mut self, op: NetworkOp, label: &str) {
        let argv = self.review.vcs.network_argv(op);
        self.pending_git = Some(GitOp {
            label: label.to_owned(),
            argv,
        });
        self.info(format!("running git {label}…"));
    }

    /// Report a finished network op: the first non-empty output line as a
    /// success toast (label + summary), or as an error on failure. Refresh
    /// first (head/log/ahead-behind may have moved), then set the toast so a
    /// clean refresh does not clobber it.
    fn git_finished(&mut self, label: &str, ok: bool, output: &str) {
        let summary = output
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_owned();
        self.message = None;
        self.refresh();
        // a refresh error already occupies the message slot; leave it
        if self.message.is_some() {
            return;
        }
        if ok {
            if summary.is_empty() {
                self.info(format!("{label} done"));
            } else {
                self.info(format!("{label}: {summary}"));
            }
        } else if summary.is_empty() {
            self.error(format!("{label} failed"));
        } else {
            self.error(summary);
        }
    }

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

    fn amend_via_editor(&mut self, use_index: bool) {
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
    fn queue_message_editor(
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
    fn apply_amend(&mut self, message: Option<&str>, use_index: bool) {
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

    fn finish_amend(&mut self, msg_path: &Path, use_index: bool, outcome: Result<bool, String>) {
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

fn tree_row_label(node: &crate::tree::TreeNode) -> String {
    match node {
        crate::tree::TreeNode::Dir { name, .. } | crate::tree::TreeNode::File { name, .. } => {
            name.clone()
        }
    }
}

fn diff_row_text(file: Option<&diffler_core::model::FileDiff>, row: &DiffRow) -> Option<String> {
    match *row {
        DiffRow::Line { hunk, line, .. } => {
            Some(file?.hunks.get(hunk)?.lines.get(line)?.text.clone())
        }
        _ => None,
    }
}

/// Byte offset of the `chars`-th character, for editing the input buffer.
fn byte_index(buffer: &str, chars: usize) -> usize {
    buffer
        .char_indices()
        .nth(chars)
        .map_or(buffer.len(), |(index, _)| index)
}

/// Two left-presses within this window (at about the same cell) are a
/// double-click.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

/// A resolved mouse interaction, screen-independent. Each screen's
/// `*_mouse` handler matches this exhaustively, so adding an interaction means
/// every screen is forced to decide how it responds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseGesture {
    /// Wheel notch over `(col, row)`.
    Scroll { col: u16, row: u16, down: bool },
    /// Single left-click: select the thing under the pointer.
    Press { col: u16, row: u16 },
    /// Double left-click: activate it (open, like `<cr>`).
    DoublePress { col: u16, row: u16 },
    /// Left-drag: extend a selection to `(col, row)`.
    Drag { col: u16, row: u16 },
    /// Right-click: cancel the in-progress interaction (e.g. drop a selection).
    Cancel,
}

/// Map a mouse point to a 0-based index into a list rendered in `area` with
/// `scroll` rows hidden above the top; `None` when the point falls outside.
pub(crate) fn hit_index(
    area: ratatui::layout::Rect,
    scroll: usize,
    col: u16,
    row: u16,
) -> Option<usize> {
    let inside =
        col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height;
    inside.then(|| scroll + (row - area.y) as usize)
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
    fn open_runs_without_a_provider_is_a_clean_noop() {
        // the fixture repo has no remote or CI config, so `o` just informs
        let (_fixture, mut app) = app();
        app.handle(key('o'));
        assert_eq!(app.screen(), Screen::Status, "no screen pushed");
        assert!(app.runs.is_empty());
        let message = app.message.expect("message");
        assert!(message.text.contains("provider"), "{}", message.text);
    }

    #[test]
    fn run_detail_event_feeds_the_graph_view() {
        use diffler_ci::{CiJob, CiRun, JobId, JobStatus, RunDetail, RunId};
        let (_fixture, mut app) = app();
        app.graph = Some(diffler_graph::GraphView::new());
        app.open_run = Some(RunId("1".into()));
        app.push_screen(Screen::Graph);
        assert_eq!(app.screen(), Screen::Graph);
        // a run detail from the poll is mapped onto the live graph
        let detail = RunDetail {
            run: CiRun {
                id: RunId("1".into()),
                name: "CI".into(),
                title: String::new(),
                branch: "main".into(),
                commit: "abc".into(),
                author: String::new(),
                created: None,
                status: JobStatus::Running,
                url: None,
            },
            jobs: vec![CiJob {
                id: JobId("lint".into()),
                name: "lint".into(),
                status: JobStatus::Ok,
                needs: vec![],
            }],
        };
        app.handle(AppEvent::CiRunDetail(detail));
        assert!(app.graph.is_some());
        // q backs out and drops the graph state
        app.handle(key('q'));
        assert_eq!(app.screen(), Screen::Status);
        assert!(app.graph.is_none());
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
    fn commit_transient_opens_on_c_and_resolves_cc() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        assert_eq!(
            app.transient.map(|t| t.kind),
            Some(TransientKind::Commit),
            "c opens the commit transient"
        );
        assert_eq!(app.message, None, "opening a transient is silent");
        app.handle(key('c'));
        assert_eq!(app.transient, None, "the leaf closes the transient");
        let request = app.pending_editor.expect("editor request");
        assert!(matches!(request.purpose, EditorPurpose::Commit { .. }));
    }

    #[test]
    fn escape_aborts_an_open_transient() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        assert!(app.transient.is_some());
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.transient, None, "esc closes without dispatching");
        assert_eq!(app.pending_editor, None);
    }

    #[test]
    fn an_unknown_key_in_a_transient_closes_it_with_a_beep() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        app.handle(key('z'));
        assert_eq!(app.transient, None);
        let message = app.message.expect("beep message");
        assert_eq!(message.severity, Severity::Info);
        assert!(message.text.contains("no such command"));
    }

    #[test]
    fn the_reveal_timer_gates_the_which_key_panel() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        assert!(app.which_key_panel().is_none(), "no flash before the tick");
        app.handle(AppEvent::Tick);
        assert!(app.which_key_panel().is_some(), "revealed after the tick");
    }

    #[test]
    fn commit_extend_amends_with_the_same_message_no_editor() {
        let (_fixture, mut app) = app();
        // ci.yml is staged in the standard fixture
        let subject_before = app.head.subject.clone();
        app.handle(key('c'));
        app.handle(key('e'));
        assert_eq!(app.pending_editor, None, "extend runs without the editor");
        assert_eq!(
            app.section_files(Section::Staged).len(),
            0,
            "index folded in"
        );
        assert_eq!(app.head.subject, subject_before, "message reused");
        let message = app.message.expect("message");
        assert!(message.text.starts_with("amended "), "{}", message.text);
    }

    #[test]
    fn commit_extend_with_nothing_staged_hints() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('c'));
        app.handle(key('e'));
        assert_eq!(app.pending_editor, None);
        assert!(
            app.message
                .expect("message")
                .text
                .contains("nothing staged")
        );
    }

    #[test]
    fn commit_amend_opens_the_editor_then_amends() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        app.handle(key('a'));
        let Some(EditorRequest {
            purpose:
                EditorPurpose::Amend {
                    msg_path,
                    use_index,
                },
            ..
        }) = app.pending_editor.take()
        else {
            panic!("expected an amend request");
        };
        assert!(use_index, "amend folds the index in");
        // the template pre-fills the existing HEAD message
        let template = std::fs::read_to_string(&msg_path).unwrap();
        assert!(template.contains("initial commit"), "{template}");
        std::fs::write(&msg_path, "reworded subject\n").unwrap();
        app.editor_finished(
            EditorPurpose::Amend {
                msg_path,
                use_index,
            },
            Ok(true),
        );
        assert_eq!(app.head.subject, "reworded subject");
        assert_eq!(
            app.section_files(Section::Staged).len(),
            0,
            "index folded in"
        );
    }

    #[test]
    fn commit_reword_changes_the_message_keeping_staged_changes() {
        let (_fixture, mut app) = app();
        app.handle(key('c'));
        app.handle(key('w'));
        let Some(EditorRequest {
            purpose:
                EditorPurpose::Amend {
                    msg_path,
                    use_index,
                },
            ..
        }) = app.pending_editor.take()
        else {
            panic!("expected an amend request");
        };
        assert!(!use_index, "reword keeps HEAD's tree");
        std::fs::write(&msg_path, "just a reword\n").unwrap();
        app.editor_finished(
            EditorPurpose::Amend {
                msg_path,
                use_index,
            },
            Ok(true),
        );
        assert_eq!(app.head.subject, "just a reword");
        // the previously staged ci.yml stays staged: reword left the tree alone
        assert!(
            app.section_files(Section::Staged)
                .iter()
                .any(|f| f.path == "ci.yml"),
            "staged change preserved across a reword"
        );
    }

    #[test]
    fn a_failed_editor_aborts_the_amend() {
        let (_fixture, mut app) = app();
        let head_before = app.head.oid7.clone();
        app.handle(key('c'));
        app.handle(key('w'));
        let request = app.pending_editor.take().expect("editor request");
        // a non-zero editor exit aborts without rewriting HEAD
        app.editor_finished(request.purpose, Ok(false));
        let message = app.message.expect("message");
        assert!(message.text.contains("amend aborted"));
        assert_eq!(app.head.oid7, head_before, "HEAD unchanged");
    }

    #[test]
    fn config_can_rebind_a_transient_sub_key() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded
            .config
            .keys
            .commit
            .insert("amend".to_owned(), "m".to_owned());
        let mut app = App::new(fixture.review(), loaded);
        app.handle(key('c'));
        app.handle(key('m'));
        let request = app.pending_editor.expect("editor request");
        assert!(matches!(
            request.purpose,
            EditorPurpose::Amend {
                use_index: true,
                ..
            }
        ));
    }

    #[test]
    fn branch_transient_creates_and_checks_out_a_branch() {
        let (_fixture, mut app) = app();
        app.handle(key('b'));
        assert_eq!(
            app.transient.map(|t| t.kind),
            Some(TransientKind::Branch),
            "b opens the branch transient"
        );
        app.handle(key('c'));
        assert_eq!(app.transient, None, "a resolving key closes the transient");
        assert!(matches!(app.modal, Some(Modal::Input { .. })));
        type_text(&mut app, "feat/x");
        app.handle(key('\n'));
        assert_eq!(app.modal, None);
        assert_eq!(app.head.branch.as_deref(), Some("feat/x"));
        let message = app.message.expect("message");
        assert!(message.text.contains("switched to new branch feat/x"));
    }

    #[test]
    fn branch_transient_n_creates_without_checkout() {
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
    fn branch_list_escape_closes_the_picker() {
        let (fixture, mut app) = app();
        fixture.branch("feat/topic");
        app.handle(key('b'));
        app.handle(key('b'));
        assert!(matches!(app.modal, Some(Modal::BranchList { .. })));
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.modal, None, "esc closes the branch picker");
    }

    #[test]
    fn push_transient_leaf_queues_the_push_argv_and_label() {
        let (_fixture, mut app) = app();
        app.handle(key('P'));
        assert_eq!(
            app.transient.map(|t| t.kind),
            Some(TransientKind::Push),
            "P opens the push transient"
        );
        app.handle(key('p'));
        assert_eq!(app.transient, None, "the leaf closes the transient");
        let git = app.pending_git.clone().expect("pending git op");
        assert_eq!(git.label, "push");
        assert_eq!(git.argv, vec!["git".to_owned(), "push".to_owned()]);
        // a running status shows immediately so the next draw reflects it
        let message = app.message.expect("running status");
        assert!(
            message.text.contains("running git push"),
            "{}",
            message.text
        );
    }

    #[test]
    fn push_set_upstream_leaf_queues_the_u_argv() {
        let (_fixture, mut app) = app();
        app.handle(key('P'));
        app.handle(key('u'));
        let git = app.pending_git.clone().expect("pending git op");
        assert_eq!(git.label, "push -u");
        assert_eq!(
            git.argv,
            vec!["git", "push", "-u", "origin", "HEAD"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pull_and_fetch_leaves_queue_their_argv() {
        let (_fixture, mut app) = app();
        app.handle(key('p'));
        app.handle(key('p'));
        assert_eq!(
            app.pending_git.take().expect("pull op").argv,
            vec!["git".to_owned(), "pull".to_owned()]
        );
        app.handle(key('f'));
        app.handle(key('f'));
        assert_eq!(
            app.pending_git.take().expect("fetch op").argv,
            vec!["git".to_owned(), "fetch".to_owned()]
        );
        app.handle(key('f'));
        app.handle(key('a'));
        assert_eq!(
            app.pending_git.take().expect("fetch-all op").argv,
            vec!["git".to_owned(), "fetch".to_owned(), "--all".to_owned()]
        );
    }

    #[test]
    fn git_done_success_shows_a_status_summary() {
        let (_fixture, mut app) = app();
        app.handle(AppEvent::GitDone {
            label: "push".to_owned(),
            ok: true,
            output: "Everything up-to-date\n".to_owned(),
        });
        let message = app.message.expect("status");
        assert_eq!(message.severity, Severity::Info);
        assert!(message.text.contains("push"), "{}", message.text);
        assert!(
            message.text.contains("Everything up-to-date"),
            "{}",
            message.text
        );
    }

    #[test]
    fn git_done_failure_surfaces_the_first_stderr_line_as_an_error() {
        let (_fixture, mut app) = app();
        app.handle(AppEvent::GitDone {
            label: "push".to_owned(),
            ok: false,
            output: "fatal: No configured push destination.\nmore detail\n".to_owned(),
        });
        let message = app.message.expect("status");
        assert_eq!(message.severity, Severity::Error);
        assert_eq!(message.text, "fatal: No configured push destination.");
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
