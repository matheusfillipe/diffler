//! Application state and event handling. `App::handle` does no terminal IO,
//! so we can unit-test the whole shell; `ui::draw` renders the state.

mod ci;
pub mod ci_log;
mod commands;
pub use commands::Command;
mod commit;
pub mod composer;
mod diff;
pub mod enrich;
mod expand;
pub mod file;
pub(crate) mod fuzzy;
pub mod image;
pub mod language;
mod log;
pub mod markdown;
mod mcp;
mod menu;
mod modal;
mod network;
pub mod pr;
pub mod pr_create;
pub mod rowsel;
mod search;
pub mod stats;
mod status;
pub mod tabs;
pub mod text_edit;
pub mod walkthrough;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(crate) use diff::RowPositions;
pub(crate) use diff::TYPING_REFUSAL;
pub use diff::lens::{Lens, LensRequest, PreviewLine, RefEntry, compute_lens};
#[cfg(test)]
pub(crate) use diff::merge_count;
pub use diff::{
    CommentFacts, CommentGrouping, CommentLine, CommentPaneRow, DeclaredRequest, DiffRow, DiffView,
    FileHighlights, FileScope, Pane, REPLY_LANE, RediffRequest, RowCopy, ScrollAlign, SplitRow,
    SplitSide, blocks_of, comment_display, folded_replies_text, group_comment_rows,
    summary_display,
};
pub use log::LogView;
pub(crate) use status::{
    BRANCHES_TITLE, CI_TITLE, PRS_TITLE, RECENT_TITLE, UNPUSHED_TITLE, WALKTHROUGHS_TITLE,
};
pub use status::{Group, Row, Section, StatusView};

use crossterm::event::{KeyCode, KeyEvent};
use diffler_core::model::DiffModel;
use diffler_core::review::Review;
use diffler_core::source::ReviewSource;
use diffler_core::vcs::{BranchInfo, HeadInfo, NetworkOp, Vcs, VcsError};

use crate::config::{Config, KeyPress, LoadedConfig};
use crate::editor::EditorRequest;
use crate::event::AppEvent;
use crate::keymap::{self, Action, Context, Keymap, Resolved};
use crate::search::Search;
use crate::theme::Theme;
use crate::transient::{Transient, TransientKind, TransientResolve};

/// What the main loop should do after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// The event left the screen as it was, so the loop skips the draw. Writes
    /// to a terminal nobody reads block the loop that answers the agent.
    Idle,
    Quit,
}

/// Screen stack entry. The per-screen state lives on `App`; the stack only
/// decides which one is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Status,
    Log,
    Diff,
    Runs,
    Graph,
    Prs,
    CiLog,
    /// One whole file, with or without its blame column.
    File,
    /// The repo's language breakdown.
    Stats,
}

impl Screen {
    fn context(self) -> Context {
        match self {
            Self::Status => Context::Status,
            // Runs is a plain list: it shares Log's motions
            Self::Runs | Self::Log => Context::Log,
            Self::CiLog => Context::CiLog,
            Self::Diff => Context::Diff,
            Self::Graph => Context::Graph,
            Self::Prs => Context::Prs,
            Self::File => Context::File,
            Self::Stats => Context::Stats,
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

/// Deferred operation a modal confirms, kept as data so `App::handle` stays a
/// pure state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOp {
    Discard {
        path: String,
    },
    DeleteBranch(String),
    /// Also removes the forge copy when synced.
    DeleteComment(String),
    DeleteAllComments,
    /// Claim every agent comment of the active review as the human's own.
    ClaimAllComments,
    DeleteWalkthrough(String),
    /// One stop of the active review's walkthrough, its primary and notes.
    DeleteStop(usize),
    /// A set-upstream push or a force-push.
    RunGit {
        label: String,
        argv: Vec<String>,
    },
    /// Discard local commits: reset --hard to `upstream`.
    ForcePull {
        upstream: String,
    },
}

/// What an input modal does with its buffer on submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOp {
    CreateBranch {
        checkout: bool,
    },
    /// One text field of the pull request being composed; we carry the draft
    /// so the form reopens with the rest of it intact.
    PrField {
        draft: Box<crate::app::pr_create::PrDraft>,
        field: crate::app::pr_create::PrField,
    },
    /// The optional top-level body of a PR review; empty submits without one.
    ReviewBody {
        number: u64,
        verdict: crate::ci::ReviewVerdict,
    },
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
    BranchList {
        branches: Vec<BranchInfo>,
        list: fuzzy::FuzzyList,
        action: BranchAction,
    },
    /// Revision picker for a three-dot review: branches or log commits.
    RevList {
        title: &'static str,
        entries: Vec<RevChoice>,
        list: fuzzy::FuzzyList,
    },
    Palette {
        list: fuzzy::FuzzyList,
    },
    /// Fuzzy picker over a fixed named choice set; applies the pick live.
    Choice {
        kind: ChoiceKind,
        list: fuzzy::FuzzyList,
    },
    RemoteList {
        remotes: Vec<String>,
        list: fuzzy::FuzzyList,
        purpose: RemotePurpose,
    },
    /// Reconcile choice when a pull finds the branch diverged from `upstream`.
    PullDiverged {
        upstream: String,
    },
    ReviewVerdict {
        number: u64,
        /// What the submit will send, resolved when the dialog opens.
        summary: Vec<String>,
    },
    CreatePr {
        draft: Box<crate::app::pr_create::PrDraft>,
    },
    /// Base-branch picker for the create form. We carry the draft so the form
    /// reopens whether or not a base was picked.
    PrBase {
        names: Vec<String>,
        list: fuzzy::FuzzyList,
        draft: Box<crate::app::pr_create::PrDraft>,
    },
    /// Fuzzy picker over every tracked file, so the reader can reach a file
    /// the review does not touch.
    FilePicker {
        paths: Vec<String>,
        list: fuzzy::FuzzyList,
    },
    /// How long a picked language holds for `path`.
    LanguageScope {
        path: String,
        language: String,
        scopes: Vec<language::LanguageScope>,
        list: fuzzy::FuzzyList,
    },
    /// The context menu of verbs that fit the thing under the pointer.
    Menu {
        commands: Vec<commands::Command>,
        list: fuzzy::FuzzyList,
    },
    /// Fuzzy picker for a project to open as a tab: the repositories near
    /// the open ones, or the folders a typed path completes to.
    AddProject {
        nearby: Vec<String>,
        entries: Vec<String>,
        list: fuzzy::FuzzyList,
    },
    Help,
}

impl Modal {
    /// The fuzzy list a dialog drives, for the pointer to move.
    pub(crate) fn list_mut(&mut self) -> Option<&mut fuzzy::FuzzyList> {
        match self {
            Self::BranchList { list, .. }
            | Self::PrBase { list, .. }
            | Self::RevList { list, .. }
            | Self::Palette { list }
            | Self::Choice { list, .. }
            | Self::FilePicker { list, .. }
            | Self::AddProject { list, .. }
            | Self::Menu { list, .. }
            | Self::LanguageScope { list, .. }
            | Self::RemoteList { list, .. } => Some(list),
            Self::Confirm { .. }
            | Self::Input { .. }
            | Self::PullDiverged { .. }
            | Self::ReviewVerdict { .. }
            | Self::CreatePr { .. }
            | Self::Help => None,
        }
    }
}

/// What choosing a remote in [`Modal::RemoteList`] does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePurpose {
    SetUpstreamPush,
    Pull,
}

/// One row of the revision picker: what to diff against, and how it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevChoice {
    pub rev: String,
    pub label: String,
}

/// A network git op the main loop runs on a blocking task, so the terminal
/// keeps drawing; the result comes back as [`AppEvent::GitDone`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOp {
    /// Status bar label, e.g. "push".
    pub label: String,
    pub argv: Vec<String>,
}

struct Keymaps {
    status: Keymap,
    diff: Keymap,
    log: Keymap,
    ci_log: Keymap,
    graph: Keymap,
    prs: Keymap,
    file: Keymap,
    stats: Keymap,
    tabs: Keymap,
}

impl Keymaps {
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
            ci_log: build(Context::CiLog),
            graph: build(Context::Graph),
            prs: build(Context::Prs),
            file: build(Context::File),
            stats: build(Context::Stats),
            tabs: build(Context::Tabs),
        }
    }
}

/// Built transients, applied with config overrides once at startup.
struct Transients {
    commit: Transient,
    branch: Transient,
    diff: Transient,
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
            TransientKind::Diff => &self.diff,
            TransientKind::Log => &self.log,
            TransientKind::Push => &self.push,
            TransientKind::Pull => &self.pull,
            TransientKind::Fetch => &self.fetch,
            TransientKind::Stash => &self.stash,
        }
    }
}

/// What the which-key panel lists.
#[derive(Debug)]
pub enum WhichKey<'a> {
    Transient(&'a Transient),
    /// The keys that finish the chord `prefix` started, beside what each does.
    Chord {
        prefix: String,
        rest: Vec<(String, &'static str)>,
    },
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

/// How long the post-refresh `↻` status-bar indicator stays up.
const REFRESH_FLASH_TICKS: u8 = 4;
/// Poll interval (in 250ms ticks) when the watcher is missing or broken.
const FALLBACK_REFRESH_TICKS: u32 = 20;
/// How often (in 250ms ticks) the wall clock behind every rendered age moves.
const CLOCK_TICKS: u32 = 40;
/// 45s, long enough to span an agent's ordinary pause between calls.
const AGENT_ACTIVITY_TTL_TICKS: u32 = 180;
/// Cap on the agent's free-text focus and file, in chars.
const AGENT_ACTIVITY_MAX_CHARS: usize = 160;

/// `DIFFLER_ACTIVITY_TTL_MS` overrides [`AGENT_ACTIVITY_TTL_TICKS`], so a test
/// can see the indicator expire without sleeping 45 seconds.
fn agent_activity_ttl_ticks() -> u32 {
    std::env::var("DIFFLER_ACTIVITY_TTL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(AGENT_ACTIVITY_TTL_TICKS, |ms| {
            ticks_in(Duration::from_millis(ms)).max(1)
        })
}

fn ticks_in(span: Duration) -> u32 {
    u32::try_from(span.as_millis() / crate::event::TICK.as_millis()).unwrap_or(u32::MAX)
}

/// One line of agent-supplied text, capped, so it can't break or flood the
/// status bar.
fn status_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(AGENT_ACTIVITY_MAX_CHARS)
        .collect()
}

/// The most recently active MCP connection's status. With several sessions
/// connected we keep only the latest report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentActivity {
    pub focus: String,
    pub file: Option<String>,
}

/// The status bar's agent-activity indicator and its expiry.
#[derive(Debug)]
pub(crate) struct AgentActivityTracker {
    pub(crate) current: Option<AgentActivity>,
    reported_at: u32,
    lasts: u32,
    ttl_ticks: u32,
}

impl AgentActivityTracker {
    fn new(ttl_ticks: u32) -> Self {
        Self {
            current: None,
            reported_at: 0,
            lasts: 0,
            ttl_ticks,
        }
    }

    fn show(&mut self, focus: &str, file: Option<&str>, lasts: u32, now: u32) {
        self.current = Some(AgentActivity {
            focus: status_text(focus),
            file: file.map(status_text),
        });
        self.reported_at = now;
        self.lasts = lasts;
    }

    fn set(&mut self, focus: &str, file: Option<&str>, now: u32) {
        self.show(focus, file, self.ttl_ticks, now);
    }

    /// `true` when this call dropped the indicator, so the caller redraws.
    fn expire(&mut self, now: u32) -> bool {
        if self.current.is_some() && now.wrapping_sub(self.reported_at) >= self.lasts {
            self.current = None;
            true
        } else {
            false
        }
    }
}

/// How much the CI poll slows while the terminal is unfocused. We poll at
/// once when focus returns.
const UNFOCUSED_POLL_FACTOR: u64 = 12;

/// What the main loop should fetch from the CI provider off-thread; the result
/// comes back as an `AppEvent`.
#[derive(Debug, Clone)]
pub enum CiRequest {
    Runs,
    Pr,
    Prs,
    PrComments(u64),
    CreatePr(Box<crate::ci::NewPullRequest>),
    Detail(crate::ci::RunId),
    Extras(crate::ci::RunId),
    Log {
        run: crate::ci::RunId,
        job: crate::ci::JobId,
        offset: u64,
    },
}

fn build_transients(
    keys: &crate::config::KeysConfig,
    startup_warnings: &mut Vec<String>,
) -> Transients {
    let mut build = |kind| {
        let (transient, warnings) = Transient::build(kind, keys);
        startup_warnings.extend(warnings);
        transient
    };
    Transients {
        commit: build(TransientKind::Commit),
        branch: build(TransientKind::Branch),
        diff: build(TransientKind::Diff),
        log: build(TransientKind::Log),
        push: build(TransientKind::Push),
        pull: build(TransientKind::Pull),
        fetch: build(TransientKind::Fetch),
        stash: build(TransientKind::Stash),
    }
}

/// Lifecycle of the off-thread repo refresh: changes queue while a worker
/// runs, so bursts collapse into at most one follow-up run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RefreshState {
    #[default]
    Idle,
    Queued,
    Running,
    RunningQueued,
}

impl RefreshState {
    #[must_use]
    pub fn queue(self) -> Self {
        match self {
            Self::Idle | Self::Queued => Self::Queued,
            Self::Running | Self::RunningQueued => Self::RunningQueued,
        }
    }

    #[must_use]
    pub fn finish(self) -> Self {
        match self {
            Self::RunningQueued => Self::Queued,
            _ => Self::Idle,
        }
    }
}

/// Work held back until the queued refresh lands, for the few flows that read
/// repo state the refresh is about to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterRefresh {
    /// Send the pull request the finished push was clearing the way for.
    CreatePr,
}

/// A git remote diffler can pull CI from: its name, the detected forge, and the
/// remote URL the provider is built from.
#[derive(Debug, Clone)]
pub struct CiRemote {
    pub name: String,
    pub detected: crate::ci::Detected,
    pub url: Option<String>,
}

/// Detect a CI provider for every git remote, one per forge. A forge whose CLI
/// isn't installed has no CI.
///
/// The first remote supplies the runs: `ci.remote`, else the remote the branch
/// pushes to, since in a fork that is the reader's own, else `origin`.
fn detect_ci_remotes(
    review: &Review,
    ci: &crate::config::CiConfig,
    pushes_to: Option<&str>,
) -> Vec<CiRemote> {
    let mut names = review.vcs.remotes().unwrap_or_default();
    preferred_first(&mut names, ci.remote.as_deref().or(pushes_to));
    let mut remotes: Vec<CiRemote> = Vec::new();
    for name in names {
        let url = review.vcs.remote_url(&name).ok().flatten();
        let Some(detected) = crate::ci::detect_for_repo(&review.repo_root, url.as_deref(), ci)
            .filter(crate::ci::provider_available)
        else {
            continue;
        };
        if remotes.iter().any(|r| r.detected == detected) {
            continue;
        }
        remotes.push(CiRemote {
            name,
            detected,
            url,
        });
    }
    remotes
}

/// Order remote names so the one whose CI the reader means comes first:
/// `preferred` when it exists, then `origin`, then the rest by name.
fn preferred_first(names: &mut [String], preferred: Option<&str>) {
    names.sort_by_key(|name| {
        (
            Some(name.as_str()) != preferred,
            name != "origin",
            name.clone(),
        )
    });
}

/// The next row `target` accepts, scanning out from `at` in one direction.
/// Every bracket-pair motion steps through this.
pub(crate) fn step_to<T>(
    rows: &[T],
    at: usize,
    forward: bool,
    target: impl Fn(&T) -> bool,
) -> Option<usize> {
    let scan = rows.iter().enumerate();
    if forward {
        scan.skip(at + 1).find(|(_, row)| target(row))
    } else {
        scan.take(at).rfind(|(_, row)| target(row))
    }
    .map(|(index, _)| index)
}

/// Cursor step for half/full page motions. Before the first render we guess a
/// typical terminal height.
pub(crate) fn page_step(viewport: u16, full: bool) -> usize {
    let lines = if viewport == 0 {
        40
    } else {
        usize::from(viewport)
    };
    if full {
        lines.saturating_sub(1).max(1)
    } else {
        (lines / 2).max(1)
    }
}

pub struct App {
    pub review: Review,
    pub head: HeadInfo,
    pub theme: Theme,
    /// Pinned to `theme`'s syntax palette; enrichment workers clone the `Arc`.
    pub highlighter: Arc<diffler_core::highlight::Highlighter>,
    pub config: Config,
    /// Author label stamped on comments and replies the human writes.
    pub author: String,
    pub screens: Vec<Screen>,
    pub status: StatusView,
    pub log: Option<LogView>,
    pub diff: Option<DiffView>,
    pub graph: Option<crate::graph::GraphView>,
    /// The resolved `click` anchors `(path, line, end)` of a card figure opened
    /// full screen. `None` while the Graph screen shows a CI run.
    pub(crate) figure_graph_anchors:
        Option<std::collections::HashMap<crate::graph::NodeId, (String, u32, u32)>>,
    /// One per distinct forge across the git remotes, computed at startup.
    pub(crate) ci_remotes: Vec<CiRemote>,
    /// Commit/range diff models the MCP handlers serve, computed once per
    /// source so agent polls never stall the render loop.
    source_models:
        std::collections::HashMap<String, std::sync::Arc<diffler_core::model::DiffModel>>,
    pub refresh_state: RefreshState,
    pub(crate) after_refresh: Option<AfterRefresh>,
    pub pending_enrich: Vec<enrich::EnrichJob>,
    /// Content hashes with a worker in flight, so bursts don't duplicate work.
    enrich_inflight: std::collections::HashSet<String>,
    /// Reusable-workflow YAML kept across provider rebuilds, so graph polls
    /// don't refetch immutable files.
    pub ci_yaml_cache: crate::ci::YamlCache,
    /// Conditional-request state per CI endpoint, so a poll that finds nothing
    /// changed costs no rate limit.
    pub ci_etags: crate::ci::EtagCache,
    /// Stays true on terminals without focus reporting.
    pub focused: bool,
    pub runs: Vec<crate::ci::CiRun>,
    /// The checked-out branch's PR, shown beside the runs section header.
    pub pr: Option<crate::ci::PullRequest>,
    /// Resolved `(merge_base, head_oid)` per opened PR, feeding its diff model.
    pub(crate) pr_ranges: std::collections::HashMap<u64, (String, String)>,
    /// A PR open waiting on its head fetch; retried when the fetch lands.
    pub(crate) pending_pr_open: Option<crate::ci::PullRequest>,
    /// A branch to switch to once its PR fetch lands.
    pub(crate) pending_pr_switch: Option<String>,
    /// A walkthrough about a PR still resolving its range, with the slide it
    /// was opened on; retried once the PR fetch or list lands.
    pub(crate) pending_walkthrough_open: Option<(String, diff::Slide)>,
    /// The agent's focus on a review still fetching.
    pub(crate) pending_focus: Option<diff::PendingFocus>,
    pub prs: Vec<crate::ci::PullRequest>,
    pub prs_cursor: usize,
    /// Scroll offsets of the two full-screen lists, so the view holds still
    /// until the cursor reaches its margin.
    pub(crate) prs_scroll: usize,
    pub(crate) runs_scroll: usize,
    /// Outbound forge posts drained by the runtime each frame.
    pub pending_pr_posts: Vec<pr::PrPost>,
    pub(crate) pr_posts_inflight: std::collections::HashSet<String>,
    /// Whether the PR has been resolved for the current branch, so we fetch it
    /// once per branch. A repo change resets it.
    pr_checked: bool,
    runs_cursor: usize,
    /// The run opened into the graph, re-polled for live status.
    open_run: Option<crate::ci::RunId>,
    /// Run ids aren't unique across forges, so we route by this remote.
    open_run_remote: Option<String>,
    pub extras: Option<crate::ci::RunExtras>,
    open_job: Option<crate::ci::JobId>,
    /// Raw job-log text and the byte offset the next poll resumes from.
    log_text: String,
    log_offset: u64,
    log_steps: Vec<crate::ci::LogStepMeta>,
    pub ci_log: Option<ci_log::CiLogView>,
    /// Stops polling; a dump-mode provider returns the whole log in one chunk.
    log_done: bool,
    pub pending_ci: Option<CiRequest>,
    pub file: Option<file::FileView>,
    pub pending_file: Option<file::FileOpen>,
    /// Bumped per file request, so we drop a load the reader has moved past.
    file_token: u64,
    pub pending_declared: Option<DeclaredRequest>,
    /// Bumped per request, so we drop an answer for a replaced file list.
    declared_token: u64,
    /// Runs on a fresh backend of its own, separate from the render loop's.
    pub pending_rediff: Option<RediffRequest>,
    /// Bumped per request, so we drop a re-diff another switch superseded.
    rediff_token: u64,
    /// Asked once at startup; halfblocks until then, and wherever the
    /// terminal answers nothing.
    pub image_picker: ratatui_image::picker::Picker,
    pub pending_image: Option<image::ImageRequest>,
    /// Bumped per request, so we drop a preview the pane has moved past.
    image_token: u64,
    /// So a draw does not ask for the same preview twice.
    image_in_flight: Option<image::ImageKey>,
    pub pending_lens: Option<LensRequest>,
    /// `gl` picks for this run as anchored globs, newest last.
    language_picks: Vec<(String, String)>,
    /// Counts highlighter rebuilds, so we drop an enrichment an older one ran.
    highlighter_generation: u64,
    /// A left press not yet let go, which opens the context menu once held.
    held_press: Option<menu::HeldPress>,
    pub pending_tab: Option<tabs::TabOp>,
    /// Set while several projects are open.
    pub tab_strip: Option<tabs::TabStrip>,
    /// The width the last frame was drawn at, for a click on the tab row.
    pub frame_width: u16,
    /// Bumped per request, so we drop a lens for a line the reader left.
    lens_token: u64,
    pub stats: Option<stats::StatsView>,
    pub pending_stats: Option<stats::StatsRequest>,
    /// Bumped per scan, so we drop an answer for a closed screen.
    stats_token: u64,
    /// Files the main loop should read so the walkthrough's anchors resolve.
    pub pending_walkthrough: Option<walkthrough::WalkthroughRequest>,
    /// Bumped per rebuild, so we drop an answer for a replaced walkthrough.
    walkthrough_token: u64,
    pub modal: Option<Modal>,
    /// Where the renderer drew the open modal's rows, for a click to find one.
    pub modal_hits: Option<crate::ui::popup::ListHits>,
    /// `search.open` means the prompt is capturing input; otherwise the
    /// highlights stay while `n`/`N` navigate.
    pub search: Option<Search>,
    pub message: Option<StatusMessage>,
    /// After the next draw the main loop emits it as OSC52 (for ssh/tmux) and
    /// also pipes it to the platform clipboard tool.
    pub pending_clipboard: Option<String>,
    /// Runs with the terminal suspended and reports back through
    /// [`App::editor_finished`].
    pub pending_editor: Option<EditorRequest>,
    /// A pull request waiting on its branch to reach the forge.
    pub pending_pr_create: Option<Box<crate::ci::NewPullRequest>>,
    pub pending_git: Option<GitOp>,
    /// Argv of the last push, so a rejection can offer a `--force-with-lease`
    /// retry against the same target.
    pub(crate) last_push_argv: Option<Vec<String>>,
    /// `None` counts as unhealthy, so the tick fallback polls.
    pub watcher_healthy: Option<Arc<AtomicBool>>,
    /// Ticks left on the status-bar `↻` indicator after a repo change.
    pub refresh_flash: u8,
    /// Bumped when the human sends feedback (`Z`) or touches a comment; the
    /// `wait_for_feedback` long-poll waits on it.
    pub feedback_tx: tokio::sync::watch::Sender<u64>,
    pub mcp_port: Option<u16>,
    pub(crate) agent_activity: AgentActivityTracker,
    keymaps: Keymaps,
    transients: Transients,
    pub transient: Option<OpenTransient>,
    pending: Vec<KeyPress>,
    pending_ticks: u8,
    tick_count: u32,
    /// Time and cell of the last left-press, for double-click detection.
    last_click: Option<(std::time::Instant, u16, u16)>,
    /// Wall-clock seconds behind every rendered age; a field so tests can pin it.
    pub now_unix: i64,
}

/// Current wall-clock time in unix seconds, or 0 before the epoch.
pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

pub(crate) fn diff_algorithm_names() -> Vec<String> {
    diffler_core::diffalgo::DiffAlgorithm::ALL
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// A named choice set [`Modal::Choice`] can pick from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKind {
    Theme,
    DiffAlgorithm,
    Language,
}

impl ChoiceKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::DiffAlgorithm => "Diff algorithm",
            Self::Language => "Language",
        }
    }

    pub fn names(self) -> Vec<String> {
        match self {
            Self::Theme => crate::theme::names(),
            Self::DiffAlgorithm => diff_algorithm_names(),
            Self::Language => language::names(),
        }
    }

    /// The row naming what is in effect now.
    pub fn current(self, app: &App) -> String {
        match self {
            Self::Theme => app.config.ui.theme.clone(),
            Self::DiffAlgorithm => app.config.diff.algorithm.to_string(),
            Self::Language => app.language_on_screen().unwrap_or_default(),
        }
    }

    fn apply(self, app: &mut App, name: &str) {
        match self {
            Self::Theme => app.apply_theme(name),
            Self::DiffAlgorithm => app.apply_diff_algorithm(name),
            Self::Language => app.open_scope_picker(name),
        }
    }
}

impl App {
    #[allow(clippy::too_many_lines)]
    pub fn new(review: Review, loaded: LoadedConfig) -> Self {
        let LoadedConfig {
            config,
            warnings: mut startup_warnings,
            ..
        } = loaded;
        // config is the source of truth, so we push it into the review's backend
        review.set_diff_algorithm(config.diff.algorithm, config.diff.indent_heuristic);
        let (theme, theme_warning) = Theme::from_name(&config.ui.theme);
        startup_warnings.extend(theme_warning);
        let highlighter = Arc::new(language::highlighter(theme.syntax, &[], &config.syntax));
        let keymaps = Keymaps::build(&config.keys, &mut startup_warnings);
        let transients = build_transients(&config.keys, &mut startup_warnings);

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
        let (unpushed, recent) = match status::load_commit_lists(
            review.vcs.as_ref(),
            config.ui.recent_commits,
            status::UNPUSHED_LIMIT,
        ) {
            Ok(lists) => lists,
            Err(err) => {
                message = Some(StatusMessage {
                    text: err.to_string(),
                    severity: Severity::Error,
                });
                (None, Vec::new())
            }
        };
        let branches = match status::load_branches(review.vcs.as_ref()) {
            Ok(branches) => branches,
            Err(err) => {
                message = Some(StatusMessage {
                    text: err.to_string(),
                    severity: Severity::Error,
                });
                Vec::new()
            }
        };
        let walkthroughs = status::load_walkthroughs(&review);

        let pushes_to = head
            .upstream
            .as_deref()
            .and_then(|upstream| upstream.split_once('/'))
            .map(|(remote, _)| remote.to_owned());
        let ci_remotes = detect_ci_remotes(&review, &config.ci, pushes_to.as_deref());

        let mut app = Self {
            review,
            head,
            theme,
            highlighter,
            config,
            author: std::env::var("USER").unwrap_or_else(|_| "you".to_owned()),
            screens: vec![Screen::Status],
            status: StatusView::new(unpushed, recent, branches, walkthroughs),
            log: None,
            diff: None,
            graph: None,
            figure_graph_anchors: None,
            // must precede `ci_remotes`, which moves the vec
            pending_ci: (!ci_remotes.is_empty()).then_some(CiRequest::Runs),
            ci_remotes,
            source_models: std::collections::HashMap::new(),
            refresh_state: RefreshState::Idle,
            after_refresh: None,
            pending_enrich: Vec::new(),
            enrich_inflight: std::collections::HashSet::new(),
            ci_yaml_cache: crate::ci::YamlCache::default(),
            ci_etags: crate::ci::EtagCache::default(),
            focused: true,
            runs: Vec::new(),
            pr: None,
            pr_checked: false,
            pr_ranges: std::collections::HashMap::new(),
            pending_pr_open: None,
            pending_pr_switch: None,
            pending_walkthrough_open: None,
            pending_focus: None,
            prs: Vec::new(),
            prs_cursor: 0,
            pending_pr_posts: Vec::new(),
            pr_posts_inflight: std::collections::HashSet::new(),
            runs_cursor: 0,
            prs_scroll: 0,
            runs_scroll: 0,
            open_run: None,
            open_run_remote: None,
            extras: None,
            open_job: None,
            log_text: String::new(),
            log_offset: 0,
            log_steps: Vec::new(),
            ci_log: None,
            log_done: false,
            modal: None,
            modal_hits: None,
            search: None,
            message,
            file: None,
            pending_file: None,
            pending_declared: None,
            stats: None,
            pending_stats: None,
            stats_token: 0,
            pending_walkthrough: None,
            walkthrough_token: 0,
            declared_token: 0,
            pending_rediff: None,
            rediff_token: 0,
            image_picker: ratatui_image::picker::Picker::halfblocks(),
            pending_image: None,
            image_token: 0,
            image_in_flight: None,
            pending_lens: None,
            pending_tab: None,
            held_press: None,
            language_picks: Vec::new(),
            highlighter_generation: 0,
            tab_strip: None,
            frame_width: 0,
            lens_token: 0,
            file_token: 0,
            pending_clipboard: None,
            pending_editor: None,
            pending_pr_create: None,
            pending_git: None,
            last_push_argv: None,
            watcher_healthy: None,
            refresh_flash: 0,
            feedback_tx: tokio::sync::watch::Sender::new(0),
            mcp_port: None,
            agent_activity: AgentActivityTracker::new(agent_activity_ttl_ticks()),
            keymaps,
            transients,
            transient: None,
            pending: Vec::new(),
            pending_ticks: 0,
            tick_count: 0,
            last_click: None,
            now_unix: now_unix(),
        };
        // with an empty branch band, row 0 is the repo-band divider
        app.clamp_cursor();
        app
    }

    /// The stack is never empty: `Back` on the last screen quits.
    pub fn screen(&self) -> Screen {
        self.screens.last().copied().unwrap_or(Screen::Status)
    }

    pub fn runs_selected(&self) -> usize {
        self.runs_cursor
    }

    pub fn log_text(&self) -> &str {
        &self.log_text
    }

    pub fn ci_log(&self) -> Option<&ci_log::CiLogView> {
        self.ci_log.as_ref()
    }

    pub fn ci_remotes(&self) -> Vec<CiRemote> {
        self.ci_remotes.clone()
    }

    pub fn open_job_name(&self) -> Option<String> {
        self.open_job.as_ref().map(|job| job.0.clone())
    }

    pub fn open_run_summary(&self) -> Option<&crate::ci::CiRun> {
        let id = self.open_run.as_ref()?;
        self.runs
            .iter()
            .find(|run| &run.id == id && run.remote == self.open_run_remote)
    }

    /// The CI remote the open run came from, else the primary one.
    pub fn ci_remote_for_open_run(&self) -> Option<CiRemote> {
        match &self.open_run_remote {
            Some(name) => self.ci_remotes.iter().find(|r| &r.name == name).cloned(),
            None => self.ci_remotes.first().cloned(),
        }
    }

    /// Keymap of the active screen, with config remaps applied.
    pub fn active_keymap(&self) -> &Keymap {
        match self.screen().context() {
            Context::Status => &self.keymaps.status,
            Context::Diff => &self.keymaps.diff,
            Context::Log => &self.keymaps.log,
            Context::CiLog => &self.keymaps.ci_log,
            Context::Graph => &self.keymaps.graph,
            Context::Prs => &self.keymaps.prs,
            Context::File => &self.keymaps.file,
            Context::Stats => &self.keymaps.stats,
            Context::Tabs => &self.keymaps.tabs,
        }
    }

    pub fn tabs_keymap(&self) -> &Keymap {
        &self.keymaps.tabs
    }

    /// Whether `key` resolves to `action` in the active keymap. The composer
    /// and the input modal intercept every key first, so they ask this to
    /// honor a configured remap.
    pub(crate) fn matches_action(&self, key: &KeyEvent, action: Action) -> bool {
        let press = keymap::press_from_event(key);
        self.active_keymap().resolve(&mut Vec::new(), press) == Resolved::Action(action)
    }

    /// Whether the path carries a current viewed mark, judged against the
    /// working-tree diff.
    pub fn is_path_viewed(&self, path: &str) -> bool {
        self.review
            .model()
            .files
            .iter()
            .find(|f| f.path == path)
            .is_some_and(|f| self.review.session.is_viewed(path, &f.content_hash()))
    }

    /// `(files in the shown diff, files marked viewed)` for the status bar,
    /// against the open diff's source (working tree when none is open).
    pub fn viewed_counts(&self) -> (usize, usize) {
        let source = self.active_review_source();
        let model = self
            .diff
            .as_ref()
            .and_then(|diff| diff.commit_model.as_ref())
            .unwrap_or_else(|| self.review.model());
        let session = self.review.session_for(&source);
        let total = model.files.len();
        let viewed = model
            .files
            .iter()
            .filter(|f| session.is_viewed(&f.path, &f.content_hash()))
            .count();
        (total, viewed)
    }

    pub(crate) fn set_agent_activity(&mut self, focus: &str, file: Option<&str>) {
        let now = self.tick_count;
        self.agent_activity.set(focus, file, now);
    }

    fn show_agent_activity(&mut self, focus: &str, file: Option<&str>, lasts: u32) {
        let now = self.tick_count;
        self.agent_activity.show(focus, file, lasts, now);
    }

    #[allow(clippy::too_many_lines)] // one arm per event
    pub fn handle(&mut self, event: AppEvent) -> Flow {
        match event {
            AppEvent::Quit => Flow::Quit,
            AppEvent::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                if self.modal.is_some() {
                    self.handle_modal_key(&key)
                } else if self.composer_open() {
                    self.handle_composer_key(&key)
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
            AppEvent::Focus(focused) => {
                self.focused = focused;
                if focused {
                    self.queue_ci_poll();
                }
                Flow::Continue
            }
            AppEvent::Tick => self.on_tick(),
            AppEvent::RefreshDone(result) => {
                self.on_refresh_done(*result);
                Flow::Continue
            }
            AppEvent::RediffDone { result, request } => self.on_rediff_done(*result, &request),
            AppEvent::ImagePreview { token, preview } => self.on_image_preview(token, *preview),
            AppEvent::Lens { token, lens } => self.on_lens(token, *lens),
            AppEvent::Enriched(outcome) => {
                self.on_enriched(*outcome);
                Flow::Continue
            }
            AppEvent::FileLoaded {
                result,
                span,
                reload,
                token,
            } => self.on_file_loaded(*result, span, reload, token),
            AppEvent::DeclaredKinds { kinds, token } => self.on_declared_kinds(kinds, token),
            AppEvent::RepoStats { stats, token } => self.on_repo_stats(*stats, token),
            AppEvent::WalkthroughAnchors {
                contents,
                pin_broken,
                token,
            } => self.on_walkthrough_anchors(&contents, pin_broken, token),
            AppEvent::CiRuns(runs) => {
                self.on_ci_runs(runs);
                Flow::Continue
            }
            AppEvent::CiPr(pr) => self.on_pr_event(pr),
            AppEvent::PrCreated(result) => {
                self.on_pr_created(*result);
                Flow::Continue
            }
            AppEvent::CiPrs(prs) => self.on_prs_event(prs),
            AppEvent::PrComments {
                number,
                comments,
                pr,
            } => self.on_pr_comments_event(number, &comments, pr),
            AppEvent::PrPosted { post, result } => self.on_pr_posted_event(&post, result),
            AppEvent::CiRunDetail(detail) => {
                self.on_run_detail(&detail);
                Flow::Continue
            }
            AppEvent::CiExtras(extras) => self.on_ci_extras(extras),
            AppEvent::CiLog {
                text,
                steps,
                next_offset,
                done,
            } => {
                self.on_ci_log(&text, steps, next_offset, done);
                Flow::Continue
            }
            AppEvent::CiError(message) => self.on_ci_error(message),
            AppEvent::CiPrsError(message) => self.on_ci_prs_error(message),
            AppEvent::RepoChanged => {
                self.queue_refresh();
                self.refresh_flash = REFRESH_FLASH_TICKS;
                // a checkout may have changed the branch; re-resolve its PR
                self.pr_checked = false;
                Flow::Continue
            }
            AppEvent::Mcp(request) => {
                // the agent gave up (say, timed out while an editor held the
                // loop), so we drop the request to avoid an unseen stale mutation
                if request.reply.is_closed() {
                    self.info("dropped stale agent request");
                } else {
                    let response = self.handle_mcp(request.kind);
                    // a dropped receiver means the agent gave up mid-call
                    let _ = request.reply.send(response);
                }
                Flow::Continue
            }
            AppEvent::McpWaiting { until } => {
                // a poll can outlast the ttl, so we count the ttl from its deadline
                let poll = ticks_in(until.saturating_duration_since(Instant::now()));
                let lasts = poll.saturating_add(self.agent_activity.ttl_ticks);
                self.show_agent_activity("waiting on you", None, lasts);
                Flow::Continue
            }
            AppEvent::GitDone { label, ok, output } => {
                self.git_finished(&label, ok, &output);
                Flow::Continue
            }
            AppEvent::Mouse(mouse) if self.modal.is_some() => {
                self.handle_modal_mouse(mouse);
                Flow::Continue
            }
            AppEvent::Mouse(mouse) if self.composer_open() => {
                self.composer_mouse(mouse);
                Flow::Continue
            }
            AppEvent::Mouse(mouse) if self.transient.is_none() => {
                self.handle_mouse(mouse);
                Flow::Continue
            }
            AppEvent::Key(_) | AppEvent::Mouse(_) | AppEvent::Resize => Flow::Continue,
        }
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        if self.screen() == Screen::Graph {
            if let Some(action) = self.graph.as_mut().and_then(|g| g.on_mouse(mouse)) {
                self.on_graph_action(&action);
            }
            return;
        }
        let (col, row) = (mouse.column, mouse.row);
        if row == 0
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(strip) = &self.tab_strip
        {
            let add_width = self
                .tabs_keymap()
                .chord_for(Action::AddProject)
                .map_or(0, |chord| {
                    unicode_width::UnicodeWidthStr::width(chord.as_str())
                        + unicode_width::UnicodeWidthStr::width(tabs::ADD_HINT)
                });
            self.pending_tab = strip.hit(
                col,
                self.frame_width,
                u16::try_from(add_width).unwrap_or(u16::MAX),
            );
            return;
        }
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
                self.held_press = Some(menu::HeldPress::new(col, row));
                if self.register_click_is_double(col, row) {
                    MouseGesture::DoublePress { col, row }
                } else {
                    MouseGesture::Press { col, row }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.held_press = None;
                MouseGesture::Drag { col, row }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.held_press = None;
                self.open_context_menu(col, row, true);
                return;
            }
            MouseEventKind::Up(_) => {
                self.held_press = None;
                return;
            }
            _ => return,
        };
        self.mouse_gesture(gesture);
    }

    fn select_at(&mut self, col: u16, row: u16) {
        self.mouse_gesture(MouseGesture::Select { col, row });
    }

    fn mouse_gesture(&mut self, gesture: MouseGesture) {
        match self.screen() {
            Screen::Status => self.status_mouse(gesture),
            Screen::Diff => self.diff_mouse(gesture),
            Screen::Log => self.log_mouse(gesture),
            Screen::CiLog => self.ci_log_mouse(gesture),
            Screen::Graph | Screen::Runs | Screen::Prs | Screen::File | Screen::Stats => {}
        }
    }

    fn register_click_is_double(&mut self, col: u16, row: u16) -> bool {
        let now = std::time::Instant::now();
        let double = self.last_click.is_some_and(|(at, c, r)| {
            now.duration_since(at) < DOUBLE_CLICK_WINDOW && c.abs_diff(col) <= 1 && r == row
        });
        self.last_click = if double { None } else { Some((now, col, row)) };
        double
    }

    fn handle_key(&mut self, key: &KeyEvent) -> Flow {
        self.held_press = None;
        if key.code == KeyCode::Esc && !self.pending.is_empty() {
            self.pending.clear();
            return Flow::Continue;
        }
        // Esc stays out of the keymap since it also drains chords and cancels modals
        if key.code == KeyCode::Esc && self.visual_active() {
            if let Some(view) = self.row_select_mut() {
                view.set_anchor(None);
            }
            self.pending.clear();
            return Flow::Continue;
        }
        if self.screen() == Screen::Diff && self.lens_active() && self.pending.is_empty() {
            match key.code {
                KeyCode::Esc => {
                    self.lens_clear();
                    return Flow::Continue;
                }
                KeyCode::Char(digit @ '1'..='9') if key.modifiers.is_empty() => {
                    self.lens_focus(usize::from(digit as u8 - b'1'));
                    return Flow::Continue;
                }
                _ => {}
            }
        }
        self.pending_ticks = 0;
        let press = keymap::press_from_event(key);
        let mut pending = std::mem::take(&mut self.pending);
        let resolved = self.active_keymap().resolve(&mut pending, press);
        self.pending = pending;
        match resolved {
            Resolved::Action(action) => self.dispatch(action),
            Resolved::Transient(TransientKind::Diff) if self.status_cursor_on_walkthrough() => {
                self.dispatch(Action::DeleteComment)
            }
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

    fn handle_transient_key(&mut self, key: &KeyEvent) -> Flow {
        let Some(open) = self.transient else {
            return Flow::Continue;
        };
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
                self.info("no such command");
                Flow::Continue
            }
        }
    }

    /// The built transient for `kind`, with config overrides applied.
    pub fn transient(&self, kind: TransientKind) -> &Transient {
        self.transients.get(kind)
    }

    /// `Some` once the reveal timer has elapsed, so a fast second key never
    /// flashes the panel.
    pub fn which_key_panel(&self) -> Option<WhichKey<'_>> {
        if let Some(open) = self.transient {
            return (self.tick_count.wrapping_sub(open.opened_at) >= WHICH_KEY_REVEAL_TICKS)
                .then(|| WhichKey::Transient(self.transients.get(open.kind)));
        }
        if self.pending.is_empty() || u32::from(self.pending_ticks) < WHICH_KEY_REVEAL_TICKS {
            return None;
        }
        Some(WhichKey::Chord {
            prefix: keymap::render_chord(&self.pending),
            rest: self.active_keymap().continuations(&self.pending),
        })
    }

    pub fn feedback_epoch(&self) -> u64 {
        *self.feedback_tx.borrow()
    }

    /// The open diff's source, else the working tree.
    pub(crate) fn active_review_source(&self) -> ReviewSource {
        self.diff
            .as_ref()
            .map_or(ReviewSource::WorkingTree, |diff| diff.source.clone())
    }

    /// Persist a human comment change and wake agents waiting for feedback.
    /// Agent mutations call [`App::persist_review_change`] so they never wake
    /// their own `wait_for_feedback` poll.
    fn after_session_change(&mut self) {
        self.feedback_tx.send_modify(|epoch| *epoch += 1);
        let source = self.active_review_source();
        let _ = self.persist_review_change(&source);
    }

    /// Persist `source`'s session and invalidate the open diff's cached rows.
    /// We toast a save error and also return it, so an MCP reply can report it.
    pub(crate) fn persist_review_change(&mut self, source: &ReviewSource) -> Result<(), String> {
        let result = self.review.save_for(source);
        if let Some(diff) = self.diff.as_mut() {
            diff.invalidate();
        }
        if let Err(err) = &result {
            self.error(err.to_string());
        }
        result.map_err(|err| err.to_string())
    }

    fn screen_shows_ages(&self) -> bool {
        matches!(self.screen(), Screen::Status | Screen::Log | Screen::Runs)
    }

    /// `Idle` when the tick left the screen untouched, so the loop skips the draw.
    fn on_tick(&mut self) -> Flow {
        let which_key = self.which_key_panel().is_some();
        self.age_pending();
        let mut changed = self.check_held_press();
        changed |= self.refresh_flash > 0;
        self.refresh_flash = self.refresh_flash.saturating_sub(1);
        self.tick_count = self.tick_count.wrapping_add(1);
        changed |= self.which_key_panel().is_some() != which_key;
        if self.tick_count.is_multiple_of(FALLBACK_REFRESH_TICKS) && self.watcher_unhealthy() {
            self.queue_refresh();
        }
        // every rendered age reads this clock; we step it coarsely and repaint
        // only a screen that shows an age
        if self.tick_count.is_multiple_of(CLOCK_TICKS) {
            let now = now_unix();
            changed |= now != self.now_unix && self.screen_shows_ages();
            self.now_unix = now;
        }
        changed |= self.agent_activity.expire(self.tick_count);
        let seconds = if self.focused {
            self.config.ci.poll_seconds.max(1)
        } else {
            self.config.ci.poll_seconds.max(1) * UNFOCUSED_POLL_FACTOR
        };
        let poll_ticks = u32::try_from(seconds.saturating_mul(4)).unwrap_or(u32::MAX);
        if self.tick_count.is_multiple_of(poll_ticks) {
            self.queue_ci_poll();
        }
        if changed { Flow::Continue } else { Flow::Idle }
    }

    fn age_pending(&mut self) {
        if !self.pending.is_empty() {
            self.pending_ticks = self.pending_ticks.saturating_add(1);
        }
    }

    pub(crate) fn dispatch(&mut self, action: Action) -> Flow {
        self.message = None;
        match action {
            Action::Quit => return Flow::Quit,
            Action::Back => return self.pop_screen(),
            Action::Refresh if self.screen() == Screen::Stats => self.rescan_stats(),
            Action::Refresh => self.queue_refresh(),
            Action::Help => self.modal = Some(Modal::Help),
            Action::SetLanguage => self.open_language_picker(),
            action if tabs::tab_op(action).is_some() => self.request_tab(action),
            Action::Palette => {
                let (_, haystack) = self.command_index_haystack();
                let mut list = fuzzy::FuzzyList::typing();
                list.rerank_words(&haystack);
                self.modal = Some(Modal::Palette { list });
            }
            Action::SendFeedback => {
                self.feedback_tx.send_modify(|epoch| *epoch += 1);
                self.info("feedback sent to waiting agents");
            }
            Action::Search => self.search_start(),
            Action::SearchNext => self.search_step_or_follow(true),
            Action::SearchPrev => self.search_step_or_follow(false),
            Action::OpenRuns => self.open_runs(),
            Action::OpenPrs => self.open_prs(),
            Action::CreatePr => self.create_pr_start(),
            Action::CommentsOverview => self.toggle_comments_sidebar(),
            Action::SwitchTheme => self.open_choice_picker(ChoiceKind::Theme),
            Action::SwitchDiffAlgorithm => self.open_choice_picker(ChoiceKind::DiffAlgorithm),
            Action::OpenFilePicker => self.open_file_picker(),
            Action::Blame => self.blame_focused(),
            Action::OpenStats => self.open_stats(),
            // the composer and the input modal handle this key first
            Action::EditExternally => {
                self.info("nothing here to edit; open a comment or a field first");
            }
            action => match self.screen() {
                Screen::Status => self.dispatch_status(action),
                Screen::Log => self.dispatch_log(action),
                Screen::Diff => self.dispatch_diff(action),
                Screen::CiLog => self.dispatch_ci_log(action),
                Screen::Runs => self.dispatch_runs(action),
                Screen::Graph => self.dispatch_graph(action),
                Screen::Prs => self.dispatch_prs(action),
                Screen::File => self.dispatch_file(action),
                Screen::Stats => self.dispatch_stats(action),
            },
        }
        Flow::Continue
    }

    /// Clears any search, since its matches are keyed to the leaving screen's rows.
    pub(crate) fn push_screen(&mut self, screen: Screen) {
        self.search = None;
        self.screens.push(screen);
    }

    fn pop_screen(&mut self) -> Flow {
        if self.screens.len() <= 1 {
            return Flow::Quit;
        }
        self.search = None;
        // so a slow file load cannot push the file view over what replaced it
        self.cancel_file_load();
        match self.screens.pop() {
            Some(Screen::Diff) => self.diff = None,
            Some(Screen::Log) => self.log = None,
            Some(Screen::File) => self.file = None,
            Some(Screen::Graph) => {
                self.graph = None;
                self.open_run = None;
                self.open_run_remote = None;
                self.extras = None;
                self.figure_graph_anchors = None;
            }
            Some(Screen::CiLog) => {
                self.open_job = None;
                self.log_text.clear();
                self.ci_log = None;
            }
            Some(Screen::Runs) => self.runs_cursor = 0,
            Some(Screen::Prs) => self.prs_cursor = 0,
            Some(Screen::Stats) => self.stats = None,
            Some(Screen::Status) | None => {}
        }
        Flow::Continue
    }

    pub(crate) fn open_choice_picker(&mut self, kind: ChoiceKind) {
        let names = kind.names();
        let current = kind.current(self);
        let mut list = match kind {
            ChoiceKind::Language => fuzzy::FuzzyList::typing(),
            ChoiceKind::Theme | ChoiceKind::DiffAlgorithm => fuzzy::FuzzyList::default(),
        };
        list.rerank(&names);
        list.selected = list
            .matches
            .iter()
            .position(|&index| names.get(index) == Some(&current))
            .unwrap_or(0);
        self.modal = Some(Modal::Choice { kind, list });
    }

    pub(crate) fn apply_theme(&mut self, name: &str) {
        let (theme, _) = Theme::from_name(name);
        self.theme = theme;
        name.clone_into(&mut self.config.ui.theme);
        self.rebuild_highlighter();
        self.info(format!("theme: {name}"));
    }

    /// Models already computed under the old algorithm are re-diffed
    /// off-thread, so a switch never blocks the render loop.
    pub(crate) fn apply_diff_algorithm(&mut self, name: &str) {
        let Some(algorithm) = diffler_core::diffalgo::DiffAlgorithm::parse(name) else {
            return;
        };
        self.config.diff.algorithm = algorithm;
        self.review
            .set_diff_algorithm(algorithm, self.config.diff.indent_heuristic);
        self.source_models.clear();
        self.queue_rediff();
        self.info(format!("diff algorithm: {algorithm}"));
    }

    /// Queue a re-diff of the status sections, the working tree, and whatever
    /// the open diff view shows.
    fn queue_rediff(&mut self) {
        let about = self
            .diff
            .as_ref()
            .map(|diff| diff.source.clone())
            .map(|source| self.resolve_about(&source));
        let pr_head = match &about {
            Some(ReviewSource::Pr { number }) => self.pr_ranges.get(number).cloned(),
            _ => None,
        };
        self.rediff_token += 1;
        self.pending_rediff = Some(RediffRequest {
            about,
            pr_head,
            token: self.rediff_token,
        });
    }

    /// Take the queued re-diff once no refresh is running. It holds the
    /// refresh slot until `on_rediff_done`, so neither it nor a
    /// refresh installs an older working tree over a newer one.
    pub fn start_rediff(&mut self) -> Option<RediffRequest> {
        if matches!(
            self.refresh_state,
            RefreshState::Running | RefreshState::RunningQueued
        ) {
            return None;
        }
        let request = self.pending_rediff.take()?;
        self.refresh_state = match self.refresh_state {
            RefreshState::Queued => RefreshState::RunningQueued,
            _ => RefreshState::Running,
        };
        Some(request)
    }

    /// Run the queued re-diff inline, mirroring `dispatch_rediff` in the runtime.
    #[cfg(test)]
    pub(crate) fn settle_rediff(&mut self) {
        let Some(request) = self.start_rediff() else {
            return;
        };
        let result = request.run(&self.review.repo_root, &self.config.diff_settings());
        self.on_rediff_done(result, &request);
    }

    /// We install the result unconditionally, since an algorithm switch
    /// changes hunks the content fingerprint cannot see.
    pub(crate) fn on_rediff_done(
        &mut self,
        result: Result<diffler_core::review::Refreshed, String>,
        request: &RediffRequest,
    ) -> Flow {
        self.refresh_state = self.refresh_state.finish();
        if request.token != self.rediff_token {
            return Flow::Idle;
        }
        let diffler_core::review::Refreshed {
            status,
            model,
            against,
            pinned,
        } = match result {
            Ok(refreshed) => refreshed,
            Err(message) => {
                self.error(message);
                return Flow::Continue;
            }
        };
        // we capture positions while the rows still match their model
        let positions = self
            .diff
            .as_ref()
            .map(|diff| diff.capture_positions(&self.review));
        let before = self.shown_hunks();
        self.review.install_refresh(status, model);
        self.status.clear_enriched();
        let Some(positions) = positions else {
            self.report_rediff(&before);
            return Flow::Continue;
        };
        let rediffed = match &request.about {
            Some(ReviewSource::Against { .. }) => against.map(|(_, result)| result),
            Some(ReviewSource::Pr { number })
                if self.pr_ranges.get(number) != request.pr_head.as_ref() =>
            {
                None
            }
            Some(
                ReviewSource::Commit { .. } | ReviewSource::Range { .. } | ReviewSource::Pr { .. },
            ) => pinned,
            _ => None,
        };
        let current = self
            .diff
            .as_ref()
            .map(|diff| diff.source.clone())
            .map(|source| self.resolve_about(&source));
        let rediffed = rediffed
            .filter(|_| current == request.about)
            .and_then(|result| result.map_err(|err| self.error(err.to_string())).ok());
        let model = match rediffed {
            Some(model) => Some(model),
            None => self.diff.as_mut().and_then(|diff| diff.commit_model.take()),
        };
        self.finish_diff_swap(positions, model);
        self.report_rediff(&before);
        Flow::Continue
    }

    /// Every file of the diff on screen with its hunk ids.
    fn shown_hunks(&self) -> Vec<(String, Vec<diffler_core::model::HunkId>)> {
        let model = match self.diff.as_ref() {
            Some(diff) => diff.model(&self.review),
            None => self.review.model(),
        };
        model
            .files
            .iter()
            .map(|file| {
                let ids = file.hunks.iter().map(|hunk| hunk.id.clone()).collect();
                (file.path.clone(), ids)
            })
            .collect()
    }

    /// Most diffs come out the same under every algorithm, so we say what a
    /// switch changed, else it looks broken.
    fn report_rediff(&mut self, before: &[(String, Vec<diffler_core::model::HunkId>)]) {
        let after = self.shown_hunks();
        let changed = after.iter().filter(|file| !before.contains(file)).count();
        let algorithm = self.config.diff.algorithm;
        self.info(match changed {
            0 if algorithm == diffler_core::diffalgo::DiffAlgorithm::Structural => {
                "structural keeps the hunks, dims reformat-only lines".to_owned()
            }
            0 => format!("{algorithm} gives the same hunks here"),
            1 => format!("{algorithm} changed the hunks of 1 file"),
            n => format!("{algorithm} changed the hunks of {n} files"),
        });
    }

    /// Install a new model into the open diff view and restore `positions`,
    /// which the caller captured before producing `model`.
    pub(crate) fn finish_diff_swap(
        &mut self,
        positions: RowPositions,
        model: Option<diffler_core::model::DiffModel>,
    ) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.commit_model = model;
        diff.invalidate();
        diff.ensure_rows(&self.review);
        diff.restore_positions(&self.review, positions);
        self.queue_declared();
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some(StatusMessage {
            text: text.into(),
            severity: Severity::Info,
        });
    }

    /// Put a resolved value on the clipboard, or name what had none.
    pub(crate) fn copy_or_report(&mut self, value: Option<String>, missing: &str) {
        let Some(value) = value else {
            self.info(format!("no URL for {missing}"));
            return;
        };
        self.info(format!("copied {value}"));
        self.pending_clipboard = Some(value);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(StatusMessage {
            text: text.into(),
            severity: Severity::Error,
        });
    }

    pub(crate) fn vcs_op(&mut self, op: impl FnOnce(&dyn Vcs) -> Result<(), VcsError>) {
        match op(self.review.vcs.as_ref()) {
            Ok(()) => self.queue_refresh(),
            Err(err) => self.error(err.to_string()),
        }
    }

    /// The refresh runs on the blocking pool, so nothing after this call sees
    /// the new state; work that needs it goes in `after_refresh`.
    pub(crate) fn queue_refresh(&mut self) {
        self.refresh_state = self.refresh_state.queue();
    }

    /// Run the queued refresh inline, mirroring `dispatch_refresh` in the runtime.
    #[cfg(test)]
    pub(crate) fn settle_refresh(&mut self) {
        if self.refresh_state != RefreshState::Queued {
            return;
        }
        self.refresh_state = RefreshState::Running;
        let against = self.against_rev().map(str::to_owned);
        let result = Review::compute_refresh(
            &self.review.repo_root,
            &self.config.diff_settings(),
            against.as_deref(),
            None,
        )
        .map_err(|err| err.to_string());
        self.on_refresh_done(result);
    }

    fn on_refresh_done(&mut self, result: Result<diffler_core::review::Refreshed, String>) {
        self.refresh_state = self.refresh_state.finish();
        match result {
            Ok(refreshed) => self.apply_refresh(refreshed),
            Err(message) => self.error(message),
        }
        if let Some(AfterRefresh::CreatePr) = self.after_refresh.take() {
            self.pr_create_after_push();
        }
    }

    pub(crate) fn apply_refresh(&mut self, refreshed: diffler_core::review::Refreshed) {
        let diffler_core::review::Refreshed {
            status,
            model,
            against,
            ..
        } = refreshed;
        self.now_unix = now_unix();
        let status_anchor = self.status_cursor_anchor();
        let diff_anchor_path = self.diff_cursor_path();
        // we capture positions before the model swap, while rows still match it
        let diff_positions = self
            .diff
            .as_ref()
            .map(|diff| diff.capture_positions(&self.review));
        // a no-op refresh keeps the old model, since the rebuild carries no
        // emphasis and would re-run enrichment for nothing
        let unchanged = self.review.model().fingerprint() == model.fingerprint();
        if unchanged {
            self.review.status = status;
        } else {
            self.review.install_refresh(status, model);
            self.status.clear_enriched();
        }
        match self.review.vcs.head() {
            Ok(head) => self.head = head,
            Err(err) => self.error(err.to_string()),
        }
        match status::load_commit_lists(
            self.review.vcs.as_ref(),
            self.config.ui.recent_commits,
            status::UNPUSHED_LIMIT,
        ) {
            Ok((unpushed, recent)) => {
                self.status.unpushed = unpushed;
                self.status.recent = recent;
            }
            Err(err) => self.error(err.to_string()),
        }
        match status::load_branches(self.review.vcs.as_ref()) {
            Ok(branches) => self.status.branches = branches,
            Err(err) => self.error(err.to_string()),
        }
        self.reload_walkthroughs();
        self.restore_status_cursor(status_anchor);
        self.refresh_log();
        let swap = against.and_then(|(rev, result)| self.against_swap(&rev, result));
        let mut moved = false;
        if let Some(diff) = self.diff.as_mut() {
            moved = !unchanged || swap.is_some();
            if let Some(model) = swap {
                diff.commit_model = Some(model);
            }
            if moved {
                diff.clear_enriched();
                diff.invalidate();
                diff.drop_lens();
            }
            diff.ensure_rows(&self.review);
        }
        // a file that joined the diff has no attributes read yet
        if moved {
            self.queue_declared();
        }
        self.restore_diff_cursor(diff_anchor_path);
        if let (Some(diff), Some(positions)) = (self.diff.as_mut(), diff_positions) {
            diff.restore_positions(&self.review, positions);
        }
    }

    /// The recomputed three-dot diff to swap into the open view, `None` when
    /// the view shows another source, the diff is unchanged, or it failed.
    fn against_swap(
        &mut self,
        rev: &str,
        result: Result<diffler_core::model::DiffModel, diffler_core::vcs::VcsError>,
    ) -> Option<diffler_core::model::DiffModel> {
        let shown = self
            .diff
            .as_ref()
            .filter(|diff| diff.source == ReviewSource::against(rev))
            .map(|diff| diff.commit_model.as_ref().map(DiffModel::fingerprint))?;
        match result {
            Ok(model) => (shown != Some(model.fingerprint())).then_some(model),
            Err(err) => {
                self.error(err.to_string());
                None
            }
        }
    }

    fn watcher_unhealthy(&self) -> bool {
        self.watcher_healthy
            .as_ref()
            .is_none_or(|healthy| !healthy.load(Ordering::Relaxed))
    }

    pub(crate) fn request_network(&mut self, op: NetworkOp, label: &str) {
        let argv = match self.review.vcs.network_argv(op) {
            Ok(argv) => argv,
            Err(err) => {
                self.error(err.to_string());
                return;
            }
        };
        let program = argv.first().map_or("git", String::as_str).to_owned();
        self.pending_git = Some(GitOp {
            label: label.to_owned(),
            argv,
        });
        self.info(format!("running {program} {label}…"));
    }

    fn on_ci_extras(&mut self, extras: crate::ci::RunExtras) -> Flow {
        self.extras = Some(extras);
        Flow::Continue
    }

    fn on_ci_error(&mut self, message: String) -> Flow {
        self.error(message);
        Flow::Continue
    }

    fn on_ci_prs_error(&mut self, message: String) -> Flow {
        self.status.prs_in_flight = false;
        // whatever waited on the list would otherwise fire on the next one
        self.pending_walkthrough_open = None;
        self.pending_focus = None;
        self.error(message);
        Flow::Continue
    }

    /// Show `pr` as the branch's own, for a PR opened from inside the session.
    pub(crate) fn seat_branch_pr(&mut self, pr: crate::ci::PullRequest) {
        let anchor = self.status_cursor_anchor();
        self.pr = Some(pr);
        self.pr_checked = true;
        self.restore_status_cursor(anchor);
    }

    fn on_pr_event(&mut self, pr: Option<crate::ci::PullRequest>) -> Flow {
        let anchor = self.status_cursor_anchor();
        self.pr = pr;
        self.pr_checked = true;
        self.restore_status_cursor(anchor);
        Flow::Continue
    }

    /// Toast a finished network op's first output line. We gate the PR
    /// continuations on the fetch's own label, since several git ops can be
    /// in flight and an unrelated one must not consume them.
    fn git_finished(&mut self, label: &str, ok: bool, output: &str) {
        if label == Self::PR_CREATE_PUSH {
            if ok {
                // the create is addressed against the pushed head
                self.queue_refresh();
                self.after_refresh = Some(AfterRefresh::CreatePr);
                return;
            }
            // the branch never reached the forge, so there is nothing to open
            self.pending_pr_create = None;
        }
        if label.starts_with(Self::PR_FETCH_PREFIX) {
            if let Some(pr) = self.pending_pr_open.take()
                && self.continue_pr_fetch(&pr, ok)
            {
                return;
            }
            if let Some(branch) = self.pending_pr_switch.take().filter(|_| ok) {
                if !self.review.vcs.native_git_checkout() {
                    self.checkout_branch(&branch);
                    return;
                }
                self.pending_git = Some(GitOp {
                    label: format!("switch {branch}"),
                    argv: vec!["git".to_owned(), "switch".to_owned(), branch],
                });
                return;
            }
        }
        let summary = output
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_owned();
        self.message = None;
        self.queue_refresh();
        if ok {
            if summary.is_empty() {
                self.info(format!("{label} done"));
            } else {
                self.info(format!("{label}: {summary}"));
            }
        } else if self.network_recovery(label, output) {
            // the recovery dialog replaces the raw error
        } else if summary.is_empty() {
            self.error(format!("{label} failed"));
        } else {
            self.error(summary);
        }
    }

    /// Finish a PR head fetch queued by `ensure_pr_range`. `false` means the
    /// fetch failed and the generic toast reports it; we drop a waiting
    /// walkthrough so it never retries a fetch that just failed.
    fn continue_pr_fetch(&mut self, pr: &crate::ci::PullRequest, ok: bool) -> bool {
        if !ok {
            self.pending_walkthrough_open = None;
            self.pending_focus = None;
            return false;
        }
        if let Some((base, head)) = self.resolve_pr_range(pr) {
            self.open_pr_diff(pr.number, &base, &head);
            if let Some((id, slide)) = self.pending_walkthrough_open.take() {
                self.open_walkthrough(&id, slide);
            }
            self.retry_focus();
        } else {
            self.pending_walkthrough_open = None;
            self.pending_focus = None;
            self.error("PR head still missing after fetch");
        }
        true
    }
}

fn tree_row_label(node: &crate::tree::TreeNode) -> String {
    match node {
        crate::tree::TreeNode::Dir { name, .. } | crate::tree::TreeNode::File { name, .. } => {
            name.clone()
        }
        // a stop row's title lives in the session, so `/` skips these rows
        crate::tree::TreeNode::Section { .. }
        | crate::tree::TreeNode::Stop { .. }
        | crate::tree::TreeNode::WalkthroughSummary => String::new(),
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

/// A screen-independent mouse interaction. Each screen's `*_mouse` handler
/// matches it exhaustively, so a new gesture makes every screen decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseGesture {
    Scroll {
        col: u16,
        row: u16,
        down: bool,
    },
    /// Select the thing under the pointer, folding a folder or group header.
    Press {
        col: u16,
        row: u16,
    },
    /// Select without folding, for the context menu to act on.
    Select {
        col: u16,
        row: u16,
    },
    /// Activate, like `<cr>`.
    DoublePress {
        col: u16,
        row: u16,
    },
    Drag {
        col: u16,
        row: u16,
    },
}

/// Index into a list drawn in `area` with `scroll` rows hidden above the top.
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
        ahead: 0,
        behind: 0,
        branch: None,
        oid7: String::new(),
        subject: String::new(),
        upstream: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::editor::EditorPurpose;

    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::test_support::{
        Fixture, ctrl_key, jj_fixture, key, standard_fixture, two_hunk_fixture,
    };
    use diffler_core::session::Anchor;

    fn app() -> (Fixture, App) {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        (fixture, app)
    }

    /// A fork has `origin` (yours) and `upstream` (theirs), and the runs worth
    /// watching are the ones for what you pushed.
    #[test]
    fn the_remote_the_branch_pushes_to_leads_the_others() {
        let order = |preferred: Option<&str>| {
            let mut names = [
                "upstream".to_owned(),
                "origin".to_owned(),
                "fork".to_owned(),
            ];
            super::preferred_first(&mut names, preferred);
            names
        };
        assert_eq!(
            order(None),
            ["origin", "fork", "upstream"],
            "origin leads when nothing else says otherwise"
        );
        assert_eq!(
            order(Some("upstream")),
            ["upstream", "origin", "fork"],
            "the branch's push target leads"
        );
        assert_eq!(
            order(Some("fork")),
            ["fork", "origin", "upstream"],
            "and so does a remote named in the config"
        );
        // a preference naming nothing present changes nothing
        assert_eq!(order(Some("nowhere")), ["origin", "fork", "upstream"]);
    }

    #[test]
    fn ci_log_mouse_scrolls_and_double_click_folds() {
        let (_fixture, mut app) = app();
        let raw = "j\tUNKNOWN STEP\t2026-06-20T00:00:00Z ##[group]Build\n\
                   j\tUNKNOWN STEP\t2026-06-20T00:00:01Z compiling\n\
                   j\tUNKNOWN STEP\t2026-06-20T00:00:02Z ##[group]Test\n\
                   j\tUNKNOWN STEP\t2026-06-20T00:00:03Z running\n";
        let mut view = ci_log::CiLogView::parse(raw, &[]);
        view.body = ratatui::layout::Rect::new(0, 1, 40, 10);
        app.ci_log = Some(view);

        app.ci_log_mouse(MouseGesture::Scroll {
            col: 1,
            row: 2,
            down: true,
        });
        assert_eq!(
            app.ci_log.as_ref().expect("view").cursor,
            1,
            "wheel moved cursor"
        );

        // double-click the Build header (top body row) unfolds it
        app.ci_log_mouse(MouseGesture::DoublePress { col: 2, row: 1 });
        let view = app.ci_log.as_ref().expect("view");
        assert_eq!(view.cursor, 0, "click re-seated the cursor on Build");
        assert!(!view.steps[0].folded, "double-click unfolded the step");
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
        // `r` on a non-comment row hints
        app.handle(key('r'));
        let message = app.message.expect("message");
        assert!(message.text.contains("comment"));
    }

    #[test]
    fn popping_the_runs_screen_keeps_the_status_ci_section() {
        use crate::ci::{CiRun, JobStatus, RunId};
        let (_fixture, mut app) = app();
        app.runs = vec![CiRun {
            id: RunId("1".into()),
            name: "CI".into(),
            title: String::new(),
            branch: "main".into(),
            commit: "abc".into(),
            author: String::new(),
            created: None,
            status: JobStatus::Ok,
            url: None,
            remote: None,
        }];
        app.push_screen(Screen::Runs);
        app.pop_screen();
        assert_eq!(app.screen(), Screen::Status);
        assert_eq!(app.runs.len(), 1, "runs survive leaving the Runs screen");
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
        use crate::ci::{CiJob, CiRun, JobId, JobStatus, RunDetail, RunId};
        let (_fixture, mut app) = app();
        app.graph = Some(crate::graph::GraphView::new());
        app.open_run = Some(RunId("1".into()));
        app.push_screen(Screen::Graph);
        assert_eq!(app.screen(), Screen::Graph);
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
                remote: None,
            },
            jobs: vec![CiJob {
                id: JobId("lint".into()),
                name: "lint".into(),
                status: JobStatus::Ok,
                duration_secs: None,
                needs: vec![],
                legs: vec![],
            }],
        };
        app.handle(AppEvent::CiRunDetail(detail));
        assert!(app.graph.is_some());
        app.handle(key('q'));
        assert_eq!(app.screen(), Screen::Status);
        assert!(app.graph.is_none());
    }

    #[test]
    fn graph_run_detail_queues_extras_once_then_stops() {
        use crate::ci::{Artifact, CiRun, JobStatus, RunDetail, RunExtras, RunId};
        let (_fixture, mut app) = app();
        app.graph = Some(crate::graph::GraphView::new());
        app.open_run = Some(RunId("1".into()));
        app.push_screen(Screen::Graph);
        let detail = || RunDetail {
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
                remote: None,
            },
            jobs: vec![],
        };
        app.handle(AppEvent::CiRunDetail(detail()));
        assert!(
            matches!(app.pending_ci, Some(CiRequest::Extras(_))),
            "first detail queues the extras fetch"
        );
        app.pending_ci = None;
        app.handle(AppEvent::CiExtras(RunExtras {
            artifacts: vec![Artifact {
                name: "a".into(),
                size_bytes: 1,
                expired: false,
            }],
            annotations: vec![],
        }));
        assert!(app.extras.is_some());
        app.handle(AppEvent::CiRunDetail(detail()));
        assert!(
            app.pending_ci.is_none(),
            "extras already present: no re-queue"
        );
        app.handle(key('q'));
        assert!(app.extras.is_none(), "leaving the graph clears extras");
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
        app.open_composer(
            crate::app::composer::ComposerKind::New {
                anchor: diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
            },
            "why?".to_owned(),
        );
        app.handle(key('\n'));
        assert_eq!(app.feedback_epoch(), 1, "comment add bumps");

        let id = app.review.session.comments[0].id.clone();
        app.open_composer(
            crate::app::composer::ComposerKind::Reply { comment_id: id },
            "because".to_owned(),
        );
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
                Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "human",
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
            project: None,
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
            project: None,
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
        app.settle_refresh();
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
    fn ctrl_g_with_nothing_focused_says_so() {
        let (_fixture, mut app) = app();
        app.handle(ctrl_key('g'));
        assert!(app.pending_editor.is_none());
        let message = app.message.expect("message");
        assert!(message.text.contains("nothing"), "{}", message.text);
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
        app.settle_refresh();
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
        app.settle_refresh();
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
        app.settle_refresh();
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
        app.settle_refresh();
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
        app.settle_refresh();
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
        app.settle_refresh();
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
        let Some(Modal::Confirm {
            message,
            on_confirm,
        }) = &app.modal
        else {
            panic!("expected a confirm modal, got {:?}", app.modal);
        };
        assert!(message.contains("feat/dead"));
        assert_eq!(*on_confirm, PendingOp::DeleteBranch("feat/dead".to_owned()));
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
    fn a_jj_repo_commits_its_whole_working_copy_through_the_editor() {
        let fixture = jj_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('c'));
        app.handle(key('c'));
        let Some(EditorRequest {
            purpose: EditorPurpose::Commit { msg_path },
            ..
        }) = app.pending_editor.take()
        else {
            panic!("expected a commit request");
        };
        std::fs::write(&msg_path, "commit everything\n").unwrap();
        app.editor_finished(EditorPurpose::Commit { msg_path }, Ok(true));
        app.settle_refresh();
        assert_eq!(app.head.subject, "commit everything");
        assert_eq!(app.section_files(Section::Staged).len(), 0);
    }

    #[test]
    fn a_jj_repo_declines_staging_a_file_and_pushing() {
        let fixture = jj_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('j'));
        app.handle(key('s'));
        let message = app.message.clone().expect("stage declined");
        assert!(message.text.contains("no staging area"), "{}", message.text);

        app.head.upstream = Some("origin/main".to_owned());
        app.handle(key('P'));
        app.handle(key('p'));
        assert!(app.pending_git.is_none());
        let message = app.message.expect("push declined");
        assert!(message.text.contains("jj git push"), "{}", message.text);
    }

    #[test]
    fn push_with_an_upstream_queues_a_plain_push() {
        let (_fixture, mut app) = app();
        app.head.upstream = Some("origin/main".to_owned());
        app.handle(key('P'));
        assert_eq!(app.transient.map(|t| t.kind), Some(TransientKind::Push));
        app.handle(key('p'));
        let git = app.pending_git.clone().expect("pending git op");
        assert_eq!(git.label, "push");
        assert_eq!(git.argv, vec!["git".to_owned(), "push".to_owned()]);
        assert!(app.message.expect("running status").text.contains("push"));
    }

    #[test]
    fn push_without_an_upstream_confirms_set_upstream_to_the_only_remote() {
        let fixture = standard_fixture();
        fixture.remote("codeberg", "https://example.invalid/r.git");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.head.upstream = None;
        app.handle(key('P'));
        app.handle(key('p'));
        assert!(
            matches!(app.modal, Some(Modal::Confirm { .. })),
            "no upstream asks first: {:?}",
            app.modal
        );
        app.confirm_modal();
        let git = app.pending_git.clone().expect("pending git op");
        assert_eq!(git.label, "push -u");
        assert_eq!(
            git.argv,
            vec!["git", "push", "-u", "codeberg", "HEAD"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            "sets upstream to the actual remote, not a hardcoded origin"
        );
    }

    #[test]
    fn push_with_multiple_remotes_opens_a_remote_chooser() {
        let fixture = standard_fixture();
        fixture.remote("origin", "https://example.invalid/a.git");
        fixture.remote("codeberg", "https://example.invalid/b.git");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.head.upstream = None;
        app.push_set_upstream();
        assert!(
            matches!(app.modal, Some(Modal::RemoteList { .. })),
            "multiple remotes ask which one: {:?}",
            app.modal
        );
    }

    #[test]
    fn a_rejected_push_offers_force_with_lease() {
        let (_fixture, mut app) = app();
        app.last_push_argv = Some(vec!["git".to_owned(), "push".to_owned()]);
        app.handle(AppEvent::GitDone {
            label: "push".to_owned(),
            ok: false,
            output: " ! [rejected]        main -> main (non-fast-forward)\n".to_owned(),
        });
        assert!(
            matches!(app.modal, Some(Modal::Confirm { .. })),
            "a non-fast-forward asks before forcing: {:?}",
            app.modal
        );
        app.confirm_modal();
        let git = app.pending_git.clone().expect("pending git op");
        assert_eq!(
            git.argv,
            vec!["git", "push", "--force-with-lease"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_rejected_force_with_lease_does_not_re_offer_force() {
        let (_fixture, mut app) = app();
        app.handle(AppEvent::GitDone {
            label: "push --force-with-lease".to_owned(),
            ok: false,
            output: "! [rejected] (stale info)\n".to_owned(),
        });
        assert_eq!(
            app.modal, None,
            "no force loop; the conflict is a real error"
        );
        assert_eq!(app.message.expect("error").severity, Severity::Error);
    }

    #[test]
    fn a_diverged_pull_offers_rebase_merge_or_force() {
        let (_fixture, mut app) = app();
        app.head.upstream = Some("origin/main".to_owned());
        app.handle(AppEvent::GitDone {
            label: "pull".to_owned(),
            ok: false,
            output:
                "hint: You have divergent branches and need to specify how to reconcile them.\n"
                    .to_owned(),
        });
        assert!(matches!(app.modal, Some(Modal::PullDiverged { .. })));
        app.handle(key('r'));
        assert_eq!(
            app.pending_git.take().expect("rebase op").argv,
            vec!["git", "pull", "--rebase"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn force_pull_needs_a_second_destructive_confirm() {
        let (_fixture, mut app) = app();
        app.modal = Some(Modal::PullDiverged {
            upstream: "origin/main".to_owned(),
        });
        app.handle(key('f'));
        assert!(
            matches!(app.modal, Some(Modal::Confirm { .. })),
            "force asks a second time before discarding: {:?}",
            app.modal
        );
        app.confirm_modal();
        assert_eq!(
            app.pending_git.take().expect("reset op").argv,
            vec!["git", "reset", "--hard", "@{u}"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn pull_with_an_upstream_queues_a_plain_pull_and_fetch_leaves_queue_argv() {
        let (_fixture, mut app) = app();
        app.head.upstream = Some("origin/main".to_owned());
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
    fn git_done_for_an_unrelated_op_leaves_a_pending_pr_continuation_alone() {
        let (_fixture, mut app) = app();
        app.pending_pr_open = Some(crate::ci::PullRequest {
            number: 7,
            title: String::new(),
            url: None,
            base_ref: "main".to_owned(),
            head_ref: "topic".to_owned(),
            head_oid: String::new(),
            author: String::new(),
        });
        // ops run detached, so a pull can finish while the PR fetch is in flight
        app.handle(AppEvent::GitDone {
            label: "pull".to_owned(),
            ok: true,
            output: "Already up to date.\n".to_owned(),
        });
        assert!(
            app.pending_pr_open.is_some(),
            "unrelated op consumed the pending PR open"
        );
        let message = app
            .message
            .take()
            .expect("the pull's own toast still shows");
        assert!(message.text.contains("pull"), "{}", message.text);
        app.handle(AppEvent::GitDone {
            label: App::pr_fetch_label(7),
            ok: true,
            output: String::new(),
        });
        assert!(
            app.pending_pr_open.is_none(),
            "the matching fetch consumes the continuation"
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
        app.settle_refresh();
        assert_eq!(app.section_files(Section::Untracked).len(), 2);
        assert_eq!(app.refresh_flash, REFRESH_FLASH_TICKS);
        app.handle(AppEvent::Tick);
        assert_eq!(app.refresh_flash, REFRESH_FLASH_TICKS - 1);
    }

    #[test]
    fn a_tick_that_paints_nothing_reports_idle_so_the_loop_skips_the_draw() {
        let (_fixture, mut app) = app();
        // the flash from opening still has frames to give
        while app.refresh_flash > 0 {
            assert_eq!(app.handle(AppEvent::Tick), Flow::Continue);
        }
        assert_eq!(app.handle(AppEvent::Tick), Flow::Idle);
    }

    #[test]
    fn the_tick_that_reveals_the_which_key_panel_asks_for_a_draw() {
        let (_fixture, mut app) = app();
        while app.handle(AppEvent::Tick) == Flow::Continue {}
        app.handle(key('b'));
        assert!(
            app.which_key_panel().is_none(),
            "the panel waits out its delay"
        );
        let mut ticks = 0;
        while app.handle(AppEvent::Tick) == Flow::Idle {
            ticks += 1;
            assert!(ticks < 20, "the panel never revealed");
        }
        assert!(app.which_key_panel().is_some());
    }

    #[test]
    fn the_tick_that_reveals_a_chords_keys_asks_for_a_draw() {
        let (_fixture, mut app) = app();
        while app.handle(AppEvent::Tick) == Flow::Continue {}
        app.handle(key('g')); // half of `gg`
        let mut ticks = 0;
        while app.handle(AppEvent::Tick) == Flow::Idle {
            ticks += 1;
            assert!(ticks < 20, "the panel never revealed");
        }
        assert!(matches!(
            app.which_key_panel(),
            Some(WhichKey::Chord { .. })
        ));
    }

    #[test]
    fn the_clock_behind_every_age_keeps_moving() {
        let (_fixture, mut app) = app();
        // a clock stopped in the past is what makes a run read "0s" forever
        app.now_unix = 0;
        for _ in 0..CLOCK_TICKS {
            app.handle(AppEvent::Tick);
        }
        assert!(
            app.now_unix > 0,
            "the tick moved the clock on: {}",
            app.now_unix
        );
    }

    #[test]
    fn a_tick_between_clock_steps_paints_nothing() {
        let (_fixture, mut app) = app();
        app.now_unix = now_unix();
        let quiet = (1..CLOCK_TICKS).all(|_| app.handle(AppEvent::Tick) == Flow::Idle);
        assert!(quiet, "idle output stays at zero between clock steps");
    }

    /// A re-diff and a refresh share one slot, so the later snapshot always
    /// lands last; a superseded re-diff still hands the slot back.
    #[test]
    fn a_rediff_waits_for_a_running_refresh_and_frees_the_slot_when_stale() {
        let (_fixture, mut app) = app();
        app.refresh_state = RefreshState::Running;
        app.apply_diff_algorithm("histogram");
        assert!(app.start_rediff().is_none(), "held behind the refresh");

        app.refresh_state = RefreshState::Idle;
        let first = app.start_rediff().expect("slot free");
        assert_eq!(app.refresh_state, RefreshState::Running);
        app.apply_diff_algorithm("patience");
        app.queue_refresh();
        let result = first.run(&app.review.repo_root, &app.config.diff_settings());
        assert_eq!(app.on_rediff_done(result, &first), Flow::Idle);
        assert_eq!(app.refresh_state, RefreshState::Queued);
    }

    #[test]
    fn agent_activity_expires_after_its_ttl_and_asks_for_a_draw() {
        let (_fixture, mut app) = app();
        app.set_agent_activity("reading the diff", None);
        for _ in 0..app.agent_activity.ttl_ticks - 1 {
            app.handle(AppEvent::Tick);
            assert!(
                app.agent_activity.current.is_some(),
                "still fresh inside the ttl"
            );
        }
        assert_eq!(
            app.handle(AppEvent::Tick),
            Flow::Continue,
            "the tick that clears it must ask for a redraw"
        );
        assert!(
            app.agent_activity.current.is_none(),
            "expired once the ttl elapsed"
        );
    }

    #[test]
    fn a_fresh_report_pushes_the_expiry_back_out() {
        let (_fixture, mut app) = app();
        app.set_agent_activity("reading the diff", None);
        for _ in 0..app.agent_activity.ttl_ticks / 2 {
            app.handle(AppEvent::Tick);
        }
        app.set_agent_activity("writing a comment", None);
        for _ in 0..app.agent_activity.ttl_ticks / 2 {
            assert!(
                app.agent_activity.current.is_some(),
                "the new report reset the ttl"
            );
            app.handle(AppEvent::Tick);
        }
    }

    #[test]
    fn waiting_outlasts_the_ttl_until_the_poll_ends() {
        let (_fixture, mut app) = app();
        let poll = std::time::Duration::from_secs(55);
        app.handle(AppEvent::McpWaiting {
            until: std::time::Instant::now() + poll,
        });
        let poll_ticks = super::ticks_in(poll);
        assert!(poll_ticks > app.agent_activity.ttl_ticks);
        for _ in 0..poll_ticks {
            app.handle(AppEvent::Tick);
        }
        let activity = app.agent_activity.current.as_ref().expect("still waiting");
        assert_eq!(activity.focus, "waiting on you");
        for _ in 0..app.agent_activity.ttl_ticks {
            app.handle(AppEvent::Tick);
        }
        assert!(
            app.agent_activity.current.is_none(),
            "ages out after the poll ends"
        );
    }

    #[test]
    fn set_agent_activity_keeps_one_line_and_caps_length() {
        let (_fixture, mut app) = app();
        let long = "x".repeat(500);
        app.set_agent_activity(&format!("line one\nline two {long}"), Some("a\nb.rs"));
        let activity = app.agent_activity.current.as_ref().expect("activity set");
        assert!(
            activity.focus.starts_with("line one line two"),
            "{}",
            activity.focus
        );
        assert_eq!(
            activity.focus.chars().count(),
            super::AGENT_ACTIVITY_MAX_CHARS
        );
        assert_eq!(activity.file.as_deref(), Some("a b.rs"));
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
        app.settle_refresh();
        assert_eq!(
            app.section_files(Section::Untracked).len(),
            1,
            "a healthy watcher means no tick polling"
        );
        healthy.store(false, Ordering::Relaxed);
        for _ in 0..FALLBACK_REFRESH_TICKS {
            app.handle(AppEvent::Tick);
        }
        app.settle_refresh();
        assert_eq!(
            app.section_files(Section::Untracked).len(),
            2,
            "the unhealthy fallback picked up the change"
        );
    }

    #[test]
    fn a_half_typed_chord_lists_the_keys_that_finish_it_until_esc() {
        let (_fixture, mut app) = app();
        app.handle(key('g'));
        assert!(app.which_key_panel().is_none(), "no flash before the tick");
        for _ in 0..8 {
            app.handle(AppEvent::Tick);
        }
        let Some(WhichKey::Chord { prefix, rest }) = app.which_key_panel() else {
            panic!("the chord panel is up");
        };
        assert_eq!(prefix, "g");
        assert!(rest.iter().any(|(keys, _)| keys == "f"), "{rest:?}");
        app.handle(AppEvent::Key(KeyEvent::from(KeyCode::Esc)));
        assert!(app.which_key_panel().is_none());
        assert_eq!(app.screen(), Screen::Status, "esc only drops the chord");
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
            on_submit: InputOp::CreateBranch { checkout: false },
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
        app.open_working_tree_diff(None);
        app.open_composer(
            crate::app::composer::ComposerKind::New {
                anchor: diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
            },
            String::new(),
        );
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
        let composer = app.diff.as_ref().and_then(|d| d.composer.as_ref());
        let composer = composer.expect("the composer is still open");
        assert_eq!(composer.buffer, "first\nsecond");
        assert_eq!(composer.cursor, 12);
        app.handle(key('\n'));
        assert!(!app.composer_open());
        assert_eq!(app.review.session.comments[0].body, "first\nsecond");
    }

    #[test]
    fn ctrl_j_is_a_newline_fallback() {
        let (_fixture, mut app) = app();
        app.modal = Some(Modal::Input {
            title: "Test".to_owned(),
            buffer: "ab".to_owned(),
            cursor: 1,
            on_submit: InputOp::CreateBranch { checkout: false },
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
            on_submit: InputOp::CreateBranch { checkout: false },
        });
        app.handle(key('\n'));
        assert_eq!(app.modal, None);
        assert_eq!(app.message, None, "no error: empty submit just closes");
    }
}
