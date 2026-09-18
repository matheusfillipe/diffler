//! Embedded MCP server: agents read review comments, answer them in place,
//! and long-poll for the human's feedback. Tool handlers never touch the
//! review directly: every call is sent through the app event channel as an
//! [`McpRequest`] and answered by `App::handle_mcp` on the main loop, so the
//! app stays the single owner of all state (no locks).

use std::fmt;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use diffler_core::feedback;
use diffler_core::model::{DiffModel, FileStatus};
use diffler_core::session::{Comment, CommentStatus};
use diffler_core::source::ReviewSource;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{PromptMessage, Role, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{
    ErrorData, Json, ServerHandler, prompt, prompt_handler, prompt_router, schemars, tool,
    tool_handler, tool_router,
};
use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::event::AppEvent;

/// Author label stamped on replies the agent writes through MCP.
pub const AGENT_AUTHOR: &str = "agent";

/// Claude Code cuts a request at 120 s, and a review pause easily outlasts
/// that: a wait plus the app round trip it still owes must return well
/// inside it. `MAX_WAIT_SECONDS` plus one `REQUEST_TIMEOUT` stays under it,
/// and the caller polls again to keep waiting.
const DEFAULT_WAIT_SECONDS: u64 = 25;
const MAX_WAIT_SECONDS: u64 = 55;
/// How long a tool call waits for the app to respond before giving up.
/// The editor suspension is the main source of delays; 30 s is generous.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One agent tool call in flight: the app answers on `reply`.
pub struct McpRequest {
    pub kind: McpRequestKind,
    pub reply: oneshot::Sender<McpResponse>,
}

impl fmt::Debug for McpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpRequest")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpRequestKind {
    ReviewStatus,
    GetDiff {
        file: Option<String>,
    },
    GetComments {
        status: Option<CommentStatus>,
    },
    /// Every persisted review (working tree, commits, ranges) with its comment
    /// counts, so the agent knows what the human reviewed and where from.
    ListReviews,
    ReplyComment {
        id: String,
        body: String,
    },
    ProposeResolve {
        id: String,
        note: Option<String>,
    },
    MarkViewed {
        file: String,
    },
    /// A new comment on `file` at `line` (through `line_end` for a range),
    /// in the review the human is currently looking at. `as_human` decides
    /// its author: the agent by default, the human when set.
    AddComment {
        file: String,
        line: u32,
        line_end: Option<u32>,
        body: String,
        as_human: bool,
    },
    /// Delete a comment the agent itself wrote. Refused for a human's own
    /// comment, or a walkthrough stop or note (`publish_walkthrough` manages
    /// those).
    DeleteComment {
        id: String,
    },
    /// Replace the body of a comment the agent itself wrote, keeping its
    /// status, replies and anchor. Same refusals as `DeleteComment`.
    EditComment {
        id: String,
        body: String,
    },
    /// Open + replied comments for `wait_for_feedback` after an epoch bump.
    Feedback,
    /// Revise the walkthrough `id` names, or publish a new one alongside any
    /// others when `id` is `None`. Refused (as [`McpResponse::Error`]) when
    /// `walkthrough_refusals` in `app/mcp.rs` finds a stop it cannot place.
    PublishWalkthrough {
        id: Option<String>,
        title: String,
        stops: Vec<StopParams>,
        skipped: Option<String>,
        summary: Option<String>,
    },
    /// The walkthrough `id` names, or the newest one when `id` is `None`.
    GetWalkthrough {
        id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpResponse {
    Status(ReviewStatusResponse),
    Diff(String),
    Comments(Vec<CommentInfo>),
    Reviews(Vec<ReviewSummary>),
    Replied {
        status: String,
    },
    Ok,
    Added {
        id: String,
    },
    /// Domain refusal (unknown id/file): surfaces as a tool error.
    Error(String),
    WalkthroughPublished(WalkthroughPublished),
    Walkthrough(Option<WalkthroughInfo>),
    /// Answers [`McpRequestKind::Feedback`]: the open and replied comments.
    /// A walkthrough stop is one of them, so its id is how the agent knows
    /// which stop the human is talking about.
    Feedback {
        comments: Vec<CommentInfo>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReviewStatusResponse {
    pub repo: String,
    pub branch: Option<String>,
    pub oid7: String,
    pub files_changed: Vec<FileEntry>,
    #[schemars(with = "Count")]
    pub open_comments: usize,
    #[schemars(with = "Count")]
    pub replied_comments: usize,
    #[schemars(with = "Count")]
    pub resolved_comments: usize,
    #[schemars(with = "Count")]
    pub feedback_epoch: u64,
    /// Every persisted review and its comment counts; `files_changed` above is
    /// the working-tree review, the default the human starts on.
    pub reviews: Vec<ReviewSummary>,
    /// Every walkthrough published in the repo, newest first; empty when none
    /// has been published.
    pub walkthroughs: Vec<WalkthroughSummary>,
    /// Filenames under `.diffler/reviews/` that failed to parse and were
    /// skipped: `reviews` and `walkthroughs` above may be missing an entry
    /// because of one of these, rather than the repository having none.
    /// Empty when every review file parsed.
    pub corrupt_reviews: Vec<String>,
}

/// What a fresh agent needs to know a walkthrough exists before reading
/// anything else: `get_walkthrough` carries the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct WalkthroughSummary {
    /// Pass this to `get_walkthrough` or `publish_walkthrough` to read or
    /// revise this walkthrough.
    pub id: String,
    pub title: String,
    #[schemars(with = "Count")]
    pub stops: usize,
    #[schemars(with = "Count")]
    pub at: u64,
}

/// One review's provenance and comment tally: what was reviewed (working tree,
/// a commit, or a range) and how many comments sit at each status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReviewSummary {
    /// Stable source key (e.g. "working", "commit-<oid>", "range-<a>-<b>").
    pub source: String,
    /// Human-facing description (e.g. "commit a1b2c3", "range a1b2c3..d4e5f6").
    pub label: String,
    #[schemars(with = "Count")]
    pub open_comments: usize,
    #[schemars(with = "Count")]
    pub replied_comments: usize,
    #[schemars(with = "Count")]
    pub resolved_comments: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct FileEntry {
    pub path: String,
    pub status: String,
    pub viewed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct DiffResponse {
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct CommentInfo {
    pub id: String,
    /// Stable key of the review this comment belongs to (see [`ReviewSummary`]).
    pub source: String,
    /// Human-facing description of that review (what the human was looking at).
    pub source_label: String,
    pub file: String,
    #[schemars(with = "Option<Count>")]
    pub line: Option<u32>,
    #[schemars(with = "Option<Count>")]
    pub line_end: Option<u32>,
    /// Which side of the diff the line numbers count: "old" or "new".
    pub side: String,
    pub body: String,
    pub status: String,
    pub author: String,
    pub replies: Vec<ReplyInfo>,
    /// The anchored line changed or vanished since the comment was made.
    pub outdated: bool,
    /// Origin-prefixed diff snippet around the anchored line.
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReplyInfo {
    pub author: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct CommentsResponse {
    pub comments: Vec<CommentInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReviewsResponse {
    pub reviews: Vec<ReviewSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReplyResponse {
    pub ok: bool,
    pub status: String,
}

/// One line of what `publish_walkthrough` checked or resolved. A refusal
/// (`too_many_stops`, `empty_stops`, `body_too_long`, `total_too_long`,
/// `anchor_unparsed`, `anchor_file_missing`, `nothing_to_anchor`,
/// `note_outside_stop`, `duplicate_id`) means nothing was stored; the rest
/// (`anchor_whole`, `figure_dropped`, `figure_simplified`) land alongside a
/// stored walkthrough so the agent learns what to fix without a broken
/// figure ever reaching the reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReceiptInfo {
    #[schemars(with = "Option<Count>")]
    pub stop: Option<usize>,
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct WalkthroughPublished {
    pub id: String,
    #[schemars(with = "Count")]
    pub stops: usize,
    pub receipts: Vec<ReceiptInfo>,
    /// The full commit this walkthrough is pinned to, `None` when the repo
    /// has no commits yet. Revise it once this no longer matches the branch.
    pub rev: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct StopInfo {
    /// The comment this stop is. Pass it back as a stop's `id` to revise the
    /// stop and keep the thread hanging off it.
    pub id: String,
    pub title: String,
    pub anchor: Option<String>,
    pub body: String,
    /// The extra comments this stop carries in its region. Pass a note's `id`
    /// back to keep it; a note whose id you leave out is deleted.
    pub notes: Vec<NoteInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct NoteInfo {
    pub id: String,
    pub anchor: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct WalkthroughInfo {
    pub id: String,
    pub title: String,
    pub author: String,
    #[schemars(with = "Count")]
    pub at: u64,
    pub skipped: Option<String>,
    /// The walkthrough's own overview, when it has one: pass it back
    /// unchanged on a revision to keep it.
    pub summary: Option<String>,
    /// The full commit this walkthrough is pinned to, `None` for one
    /// published before `rev` existed.
    pub rev: Option<String>,
    pub stops: Vec<StopInfo>,
}

/// An MCP tool's structured output must itself describe an object, so a
/// possibly-absent walkthrough is a field here rather than the bare
/// `Option<WalkthroughInfo>` a top-level return would need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct GetWalkthroughResponse {
    pub walkthrough: Option<WalkthroughInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct WaitForFeedbackResponse {
    #[schemars(with = "Count")]
    pub epoch: u64,
    pub timed_out: bool,
    pub comments: Vec<CommentInfo>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDiffParams {
    /// Restrict the diff to one file (repo-relative path).
    pub file: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetCommentsParams {
    /// Filter by comment status: "open", "replied", or "resolved".
    pub status: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReplyCommentParams {
    pub id: String,
    pub body: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProposeResolveParams {
    pub id: String,
    /// Short note on why the comment is addressed.
    pub note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MarkViewedParams {
    /// Repo-relative path of a file in the review diff.
    pub file: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddCommentParams {
    /// Repo-relative path of a file in the review diff.
    pub file: String,
    /// Line number in the diff (new-side, unless the line was deleted).
    #[schemars(with = "Count")]
    pub line: u32,
    /// Last line of an inclusive range starting at `line`; omit for one line.
    #[schemars(with = "Option<Count>")]
    pub line_end: Option<u32>,
    pub body: String,
    /// Author the comment as the human, so it goes out untouched with their
    /// next submitted review. Off by default, which leaves the comment the
    /// agent's own for the human to answer.
    pub as_human: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct AddCommentResponse {
    pub id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteCommentParams {
    /// The comment to delete. Must be the agent's own, and not a walkthrough
    /// stop or note.
    pub id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EditCommentParams {
    /// The comment to rewrite. Must be the agent's own, and not a
    /// walkthrough stop or note.
    pub id: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
pub struct StopParams {
    /// The comment id of the stop this replaces, from `get_walkthrough`. Pass
    /// it to keep that stop's thread; omit it for a new stop.
    pub id: Option<String>,
    pub title: String,
    /// `path#symbol` where a symbol exists, `path:start-end` where none does,
    /// `path:line` for one line, a bare `path` for a whole file, or omit for
    /// a stop with no anchor.
    pub anchor: Option<String>,
    pub body: String,
    /// Extra remarks on this same stop: a second point about another part of
    /// its region, or a diagram beside the prose. Each becomes its own comment
    /// in the stop's slide.
    pub notes: Option<Vec<NoteParams>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
pub struct NoteParams {
    /// The comment id of the note this replaces, from `get_walkthrough`. Pass
    /// it to keep that note's thread; omit it for a new note.
    pub id: Option<String>,
    /// `path:line` or `path:start-end` in the stop's own file, or omit to
    /// anchor the note at the start of the stop's region.
    pub anchor: Option<String>,
    pub body: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PublishWalkthroughParams {
    /// The walkthrough to revise, from `get_walkthrough` or `review_status`.
    /// Omit to publish a new one of its own.
    pub id: Option<String>,
    pub title: String,
    pub stops: Vec<StopParams>,
    /// One line on what you left out and why.
    pub skipped: Option<String>,
    /// What the reader meets first: one short paragraph saying what the
    /// change does, plus one `mermaid` flowchart of the simplest shape that
    /// explains it, five to eight nodes, naming real files or functions.
    /// Never a list of the stops or a repeat of their titles. Omit for a
    /// walkthrough with no summary.
    pub summary: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetWalkthroughParams {
    /// The walkthrough to fetch, from `review_status`. Omit for the newest.
    pub id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaitForFeedbackParams {
    /// Return once the feedback epoch exceeds this value. Defaults to the
    /// current epoch, i.e. wait for the next human send.
    #[schemars(with = "Option<Count>")]
    pub since_epoch: Option<u64>,
    /// Long-poll timeout in seconds (default 25), capped at 55; call again
    /// to keep waiting.
    #[schemars(with = "Option<Count>")]
    pub timeout_seconds: Option<u64>,
}

/// Schema stand-in for every unsigned field in the tool types. `usize`/`u32`/
/// `u64` derive `format: "uint"`/`"uint32"`/`"uint64"`, none of which JSON
/// Schema registers, so strict validators warn on every tool schema. rmcp
/// hardcodes its generator, leaving `#[schemars(with = "Count")]` (or
/// `Option<Count>`, which keeps the field optional) as the per-field opt-out.
struct Count;

impl schemars::JsonSchema for Count {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Count".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "diffler::mcp::Count".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "integer", "minimum": 0 })
    }
}

pub const fn comment_status_name(status: CommentStatus) -> &'static str {
    match status {
        CommentStatus::Open => "open",
        CommentStatus::Replied => "replied",
        CommentStatus::Resolved => "resolved",
    }
}

pub const fn file_status_name(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "added",
        FileStatus::Modified => "modified",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        FileStatus::Untracked => "untracked",
        FileStatus::Unchanged => "unchanged",
    }
}

/// Unified-style text rendering of a diff model for `get_diff`.
pub fn render_unified(model: &DiffModel, file: Option<&str>) -> Result<String, String> {
    use std::fmt::Write as _;

    if let Some(path) = file
        && !model.files.iter().any(|f| f.path == path)
    {
        return Err(format!("unknown file: {path}"));
    }
    let mut out = String::new();
    for f in &model.files {
        if file.is_some_and(|path| path != f.path) {
            continue;
        }
        let old = match f.status {
            FileStatus::Added | FileStatus::Untracked => "/dev/null".to_owned(),
            _ => format!("a/{}", f.old_path.as_deref().unwrap_or(&f.path)),
        };
        let new = match f.status {
            FileStatus::Deleted => "/dev/null".to_owned(),
            _ => format!("b/{}", f.path),
        };
        let _ = writeln!(out, "--- {old}");
        let _ = writeln!(out, "+++ {new}");
        if f.binary {
            out.push_str("Binary files differ\n");
            continue;
        }
        for hunk in &f.hunks {
            let _ = writeln!(out, "{}", hunk.header());
            for line in &hunk.lines {
                let _ = writeln!(out, "{}{}", line.kind.origin(), line.text);
            }
        }
    }
    Ok(out)
}

/// Agent-facing view of one comment, with context and outdated detection
/// judged against the current diff model, tagged with its review source.
pub fn comment_info(comment: &Comment, model: &DiffModel, source: &ReviewSource) -> CommentInfo {
    let anchor = &comment.anchor;
    // range comments anchor to their END line (`Anchor::is_outdated`), but
    // the context snippet renders from the START line so it reads naturally
    let context = anchor
        .line
        .and_then(|line| feedback::context_snippet(model, &anchor.file, line, anchor.on_old_side))
        .map(|snippet| {
            snippet
                .iter()
                .map(|(origin, text)| format!("{origin}{text}"))
                .collect::<Vec<_>>()
                .join("\n")
        });
    let outdated = anchor.is_outdated(model);
    CommentInfo {
        id: comment.id.clone(),
        source: source.key(),
        source_label: source.label(),
        file: anchor.file.clone(),
        line: anchor.line,
        line_end: anchor.line_end,
        side: if anchor.on_old_side { "old" } else { "new" }.to_owned(),
        body: comment.body.clone(),
        status: comment_status_name(comment.status).to_owned(),
        author: comment.author.clone(),
        replies: comment
            .replies
            .iter()
            .map(|r| ReplyInfo {
                author: r.author.clone(),
                body: r.body.clone(),
            })
            .collect(),
        outdated,
        context,
    }
}

/// MCP tool handler: forwards every call to the app over the event channel
/// and holds a feedback-epoch receiver for the long poll.
#[derive(Clone)]
pub struct DifflerMcp {
    tx: UnboundedSender<AppEvent>,
    feedback_rx: watch::Receiver<u64>,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

fn mismatch() -> ErrorData {
    ErrorData::internal_error("unexpected response from the diffler app", None)
}

fn parse_status(status: &str) -> Result<CommentStatus, ErrorData> {
    match status {
        "open" => Ok(CommentStatus::Open),
        "replied" => Ok(CommentStatus::Replied),
        "resolved" => Ok(CommentStatus::Resolved),
        other => Err(ErrorData::invalid_params(
            format!("unknown status {other:?} (expected open, replied, or resolved)"),
            None,
        )),
    }
}

impl DifflerMcp {
    pub fn new(tx: UnboundedSender<AppEvent>, feedback_rx: watch::Receiver<u64>) -> Self {
        Self {
            tx,
            feedback_rx,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    async fn request(&self, kind: McpRequestKind) -> Result<McpResponse, ErrorData> {
        self.request_with_timeout(kind, REQUEST_TIMEOUT).await
    }

    async fn request_with_timeout(
        &self,
        kind: McpRequestKind,
        timeout: Duration,
    ) -> Result<McpResponse, ErrorData> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(AppEvent::Mcp(McpRequest { kind, reply }))
            .map_err(|_| ErrorData::internal_error("the diffler TUI is not running", None))?;
        match tokio::time::timeout(timeout, response).await {
            Ok(Ok(McpResponse::Error(message))) => Err(ErrorData::invalid_params(message, None)),
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(ErrorData::internal_error(
                "the diffler TUI dropped the request",
                None,
            )),
            Err(_) => Err(ErrorData::internal_error(
                "diffler is busy (editor open or loop stalled), retry",
                None,
            )),
        }
    }
}

#[tool_router]
impl DifflerMcp {
    #[tool(
        description = "Current review state: repo, branch, changed files with viewed marks, comment counts, and the feedback epoch for wait_for_feedback."
    )]
    async fn review_status(&self) -> Result<Json<ReviewStatusResponse>, ErrorData> {
        match self.request(McpRequestKind::ReviewStatus).await? {
            McpResponse::Status(status) => Ok(Json(status)),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Unified diff text of the working tree under review, optionally restricted to one file."
    )]
    async fn get_diff(
        &self,
        Parameters(params): Parameters<GetDiffParams>,
    ) -> Result<Json<DiffResponse>, ErrorData> {
        let kind = McpRequestKind::GetDiff { file: params.file };
        match self.request(kind).await? {
            McpResponse::Diff(diff) => Ok(Json(DiffResponse { diff })),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Review comments across every review (working tree, commits, ranges), each tagged with its source and source_label; anchors, diff context, and threads included. Optionally filtered by status (open, replied, resolved)."
    )]
    async fn get_comments(
        &self,
        Parameters(params): Parameters<GetCommentsParams>,
    ) -> Result<Json<CommentsResponse>, ErrorData> {
        let status = params.status.as_deref().map(parse_status).transpose()?;
        match self.request(McpRequestKind::GetComments { status }).await? {
            McpResponse::Comments(comments) => Ok(Json(CommentsResponse { comments })),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "List every review the human has: the working tree, individual commits, and commit ranges, each with its comment counts, so you can tell where feedback came from."
    )]
    async fn list_reviews(&self) -> Result<Json<ReviewsResponse>, ErrorData> {
        match self.request(McpRequestKind::ListReviews).await? {
            McpResponse::Reviews(reviews) => Ok(Json(ReviewsResponse { reviews })),
            _ => Err(mismatch()),
        }
    }

    #[tool(description = "Answer a review comment in place; the human sees the reply immediately.")]
    async fn reply_comment(
        &self,
        Parameters(params): Parameters<ReplyCommentParams>,
    ) -> Result<Json<ReplyResponse>, ErrorData> {
        let kind = McpRequestKind::ReplyComment {
            id: params.id,
            body: params.body,
        };
        match self.request(kind).await? {
            McpResponse::Replied { status } => Ok(Json(ReplyResponse { ok: true, status })),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Flag a comment as addressed: marks it replied. Writes nothing into the thread, so call it after reply_comment rather than repeating the answer. The optional note is used only when you did not reply. Only the human can resolve it, in the TUI."
    )]
    async fn propose_resolve(
        &self,
        Parameters(params): Parameters<ProposeResolveParams>,
    ) -> Result<Json<ReplyResponse>, ErrorData> {
        let kind = McpRequestKind::ProposeResolve {
            id: params.id,
            note: params.note,
        };
        match self.request(kind).await? {
            McpResponse::Replied { status } => Ok(Json(ReplyResponse { ok: true, status })),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Mark a file as viewed in the review the human is currently looking at (working tree, or the open commit/range diff)."
    )]
    async fn mark_viewed(
        &self,
        Parameters(params): Parameters<MarkViewedParams>,
    ) -> Result<Json<OkResponse>, ErrorData> {
        let kind = McpRequestKind::MarkViewed { file: params.file };
        match self.request(kind).await? {
            McpResponse::Ok => Ok(Json(OkResponse { ok: true })),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Write a new review comment on a line or an inclusive line range of a file, in the review the human is currently looking at (working tree, or the open commit/range/PR diff). Anchored to that line exactly like a human's own comment, so a rewrite marks it outdated the same way. Authored as the agent by default, so the human answers it in the thread; pass as_human to author it as the human's own instead, so it goes out untouched with their next submitted review."
    )]
    async fn add_comment(
        &self,
        Parameters(params): Parameters<AddCommentParams>,
    ) -> Result<Json<AddCommentResponse>, ErrorData> {
        let kind = McpRequestKind::AddComment {
            file: params.file,
            line: params.line,
            line_end: params.line_end,
            body: params.body,
            as_human: params.as_human.unwrap_or(false),
        };
        match self.request(kind).await? {
            McpResponse::Added { id } => Ok(Json(AddCommentResponse { id })),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Delete a comment you wrote with add_comment. Refused for a human's own comment, and for a walkthrough stop or note (revise or drop those with publish_walkthrough instead)."
    )]
    async fn delete_comment(
        &self,
        Parameters(params): Parameters<DeleteCommentParams>,
    ) -> Result<Json<OkResponse>, ErrorData> {
        let kind = McpRequestKind::DeleteComment { id: params.id };
        match self.request(kind).await? {
            McpResponse::Ok => Ok(Json(OkResponse { ok: true })),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Replace the body of a comment you wrote with add_comment, keeping its status, replies and anchor. Refused for a human's own comment, and for a walkthrough stop or note (revise those with publish_walkthrough instead)."
    )]
    async fn edit_comment(
        &self,
        Parameters(params): Parameters<EditCommentParams>,
    ) -> Result<Json<OkResponse>, ErrorData> {
        let kind = McpRequestKind::EditComment {
            id: params.id,
            body: params.body,
        };
        match self.request(kind).await? {
            McpResponse::Ok => Ok(Json(OkResponse { ok: true })),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Publish the reading order for a change, one stop per real decision and as few as the change needs (five is common, ten is a lot, more means the change wants splitting). Pass `id` (from `review_status` or `get_walkthrough`) to revise that walkthrough in place; omit it to publish a new one alongside any others already on the review, when this change is a different one from those. Every stop answers why, never restates what the diff already shows. Write the body as two to four bullets, each a full plain sentence starting with `We`, every identifier in backticks, no metaphor for code (raise, return, check; never gate, carry, land). A stop is an agent comment, so the human replies to it in the thread. Anchor with `path#symbol` where a symbol exists, `path:start-end` where none does, `path:line` for one line, a bare `path` for a whole file, or omit the anchor for an overview stop. A stop is a topic, and its `notes` are the extra remarks on it: a second point about another part of its region, or a diagram beside the prose, each anchored in the stop's own file. When you revise a walkthrough, pass each surviving stop's and note's `id` from `get_walkthrough` so it keeps its comment and the replies on it; one whose id you leave out is deleted. Put what you left out in `skipped`. The reply carries the walkthrough's `id` and validation receipts; a refusal names what to fix and republish."
    )]
    async fn publish_walkthrough(
        &self,
        Parameters(params): Parameters<PublishWalkthroughParams>,
    ) -> Result<Json<WalkthroughPublished>, ErrorData> {
        let kind = McpRequestKind::PublishWalkthrough {
            id: params.id,
            title: params.title,
            stops: params.stops,
            skipped: params.skipped,
            summary: params.summary,
        };
        match self.request(kind).await? {
            McpResponse::WalkthroughPublished(published) => Ok(Json(published)),
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "The walkthrough `id` names (from review_status), or the newest one when `id` is omitted, or null when the review has none."
    )]
    async fn get_walkthrough(
        &self,
        Parameters(params): Parameters<GetWalkthroughParams>,
    ) -> Result<Json<GetWalkthroughResponse>, ErrorData> {
        match self
            .request(McpRequestKind::GetWalkthrough { id: params.id })
            .await?
        {
            McpResponse::Walkthrough(walkthrough) => {
                Ok(Json(GetWalkthroughResponse { walkthrough }))
            }
            _ => Err(mismatch()),
        }
    }

    #[tool(
        description = "Long-poll until the human sends feedback (comments, replies, or the send key). Returns the new epoch and all open/replied comments, or timed_out. A timed_out result means the human is still reviewing: call again with the epoch it returned to keep waiting. A comment on a walkthrough stop arrives as a reply on that stop's own comment, so its id names the stop."
    )]
    async fn wait_for_feedback(
        &self,
        Parameters(params): Parameters<WaitForFeedbackParams>,
    ) -> Result<Json<WaitForFeedbackResponse>, ErrorData> {
        let mut rx = self.feedback_rx.clone();
        let since = params.since_epoch.unwrap_or_else(|| *rx.borrow());
        let timeout = Duration::from_secs(
            params
                .timeout_seconds
                .unwrap_or(DEFAULT_WAIT_SECONDS)
                .min(MAX_WAIT_SECONDS),
        );
        let waited = tokio::time::timeout(timeout, rx.wait_for(|epoch| *epoch > since))
            .await
            // copy the epoch out so the watch borrow ends before the match
            .map(|result| result.map(|epoch| *epoch));
        let (epoch, timed_out) = match waited {
            Ok(Ok(epoch)) => (epoch, false),
            Ok(Err(_)) => {
                return Err(ErrorData::internal_error(
                    "the diffler TUI is not running",
                    None,
                ));
            }
            Err(_) => (*rx.borrow(), true),
        };
        if timed_out {
            return Ok(Json(WaitForFeedbackResponse {
                epoch,
                timed_out: true,
                comments: Vec::new(),
            }));
        }
        match self.request(McpRequestKind::Feedback).await? {
            McpResponse::Feedback { comments } => Ok(Json(WaitForFeedbackResponse {
                epoch,
                timed_out: false,
                comments,
            })),
            _ => Err(mismatch()),
        }
    }
}

/// The steps below a skill file's `---` frontmatter, trimmed. The `review`
/// and `walkthrough` prompts below reuse `skills/df/SKILL.md` and
/// `skills/dfa/SKILL.md` verbatim through this, so the prompt and the skill
/// cannot drift apart.
/// A skill file's prose, its YAML frontmatter dropped. A Windows checkout
/// carries CRLF, so the newlines are normalised first: matching on `\n` alone
/// finds no frontmatter there and ships the whole file, YAML header included,
/// to the agent.
fn skill_body(doc: &str) -> String {
    let doc = doc.replace("\r\n", "\n");
    let body = doc
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(doc.as_str(), |(_, body)| body);
    body.trim().to_owned()
}

// a published crate carries only its own directory, so the prompts live here
// and `agent_command_sync` pins them to the skill files the plugin ships
const DF_SKILL: &str = include_str!("../prompts/df.md");

const DFA_SKILL: &str = include_str!("../prompts/dfa.md");

const DFR_SKILL: &str = include_str!("../prompts/dfr.md");

/// Clients surface MCP prompts as commands (Claude Code renders this as
/// `/diffler:review`), so connected agents get a one-keystroke entry into
/// the review loop.
#[prompt_router]
impl DifflerMcp {
    #[prompt(
        name = "review",
        description = "Check the diffler review: read the human's open comments, address them in code, reply, and wait for the next round."
    )]
    async fn review(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(Role::User, skill_body(DF_SKILL))]
    }

    #[prompt(
        name = "walkthrough",
        description = "Walk the human through a change: publish a walkthrough, one stop per real decision, and answer their comments."
    )]
    async fn walkthrough(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(Role::User, skill_body(DFA_SKILL))]
    }

    #[prompt(
        name = "critique",
        description = "Review a change in diffler: read the diff and leave comments on real problems, one per issue, and never submit."
    )]
    async fn critique(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(Role::User, skill_body(DFR_SKILL))]
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for DifflerMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_instructions(
            "diffler code review: get_comments reads the human's diff comments, \
             reply_comment answers them in place, propose_resolve flags an \
             answered one as addressed without adding text, and \
             wait_for_feedback long-polls until the human sends \
             new feedback. The review prompt packages that loop as a command.",
        )
    }
}

pub struct McpHandle {
    /// Actual bound port: the configured one, or an ephemeral fallback.
    pub port: u16,
    pub handle: JoinHandle<()>,
}

// SO_REUSEADDR lets a restart reclaim the port while the old socket lingers in
// TIME_WAIT. Unix only: on Windows it instead allows hijacking a live port and
// errors WSAEACCES, defeating the busy-port fallback.
fn bind_reusable(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let socket = tokio::net::TcpSocket::new_v4()?;
    #[cfg(unix)]
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(1024)
}

/// Serve the MCP tools over streamable HTTP at `127.0.0.1:{port}/mcp`.
/// A taken port falls back to an ephemeral one instead of failing the TUI;
/// the returned handle carries the port that actually bound.
pub fn spawn_mcp(
    tx: UnboundedSender<AppEvent>,
    feedback_rx: watch::Receiver<u64>,
    port: u16,
) -> std::io::Result<McpHandle> {
    let listener = match bind_reusable(SocketAddr::from(([127, 0, 0, 1], port))) {
        Ok(listener) => listener,
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            bind_reusable(SocketAddr::from(([127, 0, 0, 1], 0)))?
        }
        Err(e) => return Err(e),
    };
    let port = listener.local_addr()?.port();
    let service: StreamableHttpService<DifflerMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(DifflerMcp::new(tx.clone(), feedback_rx.clone())),
            std::sync::Arc::default(),
            StreamableHttpServerConfig::default(),
        );
    let router = axum::Router::new().nest_service("/mcp", service);
    let handle = tokio::spawn(async move {
        // serve only ends on listener errors; the TUI aborts this task on quit
        let _ = axum::serve(listener, router).await;
    });
    Ok(McpHandle { port, handle })
}

/// Repo-relative path of the endpoint discovery file an external proxy reads.
const ENDPOINT_FILE: &str = "mcp.json";

fn endpoint_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".diffler").join(ENDPOINT_FILE)
}

#[derive(Serialize)]
struct EndpointFile {
    port: u16,
    url: String,
    pid: u32,
}

/// Publish the live MCP endpoint to `.diffler/mcp.json` so a stdio proxy (the
/// `npx` bridge) can discover the actual port, which may differ from the
/// configured one after an ephemeral fallback. Also registers this instance
/// under the per-user registry, so a proxy started from a directory that owns
/// no repo of its own can still find it.
pub fn write_endpoint(repo_root: &Path, port: u16) -> std::io::Result<()> {
    let dir = repo_root.join(".diffler");
    std::fs::create_dir_all(&dir)?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n")?;
    }
    let pid = std::process::id();
    let body = serde_json::to_string_pretty(&EndpointFile {
        port,
        url: format!("http://127.0.0.1:{port}/mcp"),
        pid,
    })
    .map_err(std::io::Error::other)?;
    std::fs::write(endpoint_path(repo_root), body)?;
    write_registry_entry(repo_root, port, pid);
    Ok(())
}

/// Remove the endpoint file on shutdown, but only when it still names this
/// process's own port. A second diffler instance in the same repo overwrites
/// the file with its own port; deleting unconditionally would let whichever
/// process exits first destroy the still-running one's proxy discovery.
pub fn clear_endpoint(repo_root: &Path, port: u16) {
    let path = endpoint_path(repo_root);
    if let Ok(body) = std::fs::read_to_string(&path) {
        let current_owner = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("port").and_then(serde_json::Value::as_u64));
        if current_owner == Some(u64::from(port)) {
            let _ = std::fs::remove_file(&path);
        }
    }
    clear_registry_entry(port);
}

#[derive(Serialize)]
struct RegistryEntry<'a> {
    repo: &'a str,
    port: u16,
    pid: u32,
    url: String,
}

/// `$XDG_STATE_HOME/diffler/instances`, falling back to `~/.local/state` the
/// way `config::load` falls back to `~/.config` for `XDG_CONFIG_HOME`.
/// `DIFFLER_STATE_DIR` is a test-only override so registry tests never touch
/// a developer's real home directory.
fn registry_dir() -> Option<PathBuf> {
    let base = if let Some(dir) = non_empty_env("DIFFLER_STATE_DIR") {
        PathBuf::from(dir)
    } else if let Some(dir) = non_empty_env("XDG_STATE_HOME") {
        PathBuf::from(dir)
    } else {
        PathBuf::from(non_empty_env("HOME")?)
            .join(".local")
            .join("state")
    };
    Some(base.join("diffler").join("instances"))
}

fn non_empty_env(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key).filter(|v| !v.is_empty())
}

/// Best-effort: losing the registry entry only loses cross-directory
/// discovery, never the local `.diffler/mcp.json` a same-repo proxy already
/// relies on, so a write failure here is silent and non-fatal.
fn write_registry_entry(repo_root: &Path, port: u16, pid: u32) {
    let Some(dir) = registry_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let repo = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let Some(repo) = repo.to_str() else {
        return;
    };
    let Ok(body) = serde_json::to_string_pretty(&RegistryEntry {
        repo,
        port,
        pid,
        url: format!("http://127.0.0.1:{port}/mcp"),
    }) else {
        return;
    };
    let _ = std::fs::write(dir.join(format!("{port}.json")), body);
}

/// Mirrors `clear_endpoint`'s owner check: only remove the registry entry
/// when it still names this process's own port, so a later instance that
/// reused the same ephemeral port isn't torn down by an earlier one's exit.
fn clear_registry_entry(port: u16) {
    let Some(dir) = registry_dir() else {
        return;
    };
    let path = dir.join(format!("{port}.json"));
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    let owner_port = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("port").and_then(serde_json::Value::as_u64));
    if owner_port == Some(u64::from(port)) {
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::model::{DiffLine, FileDiff, Hunk, HunkId, LineKind};
    use diffler_core::session::{Anchor, Session};

    use super::*;

    fn diff_line(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: &str) -> DiffLine {
        DiffLine::new(kind, old_no, new_no, text.to_owned())
    }

    fn sample_model() -> DiffModel {
        DiffModel {
            files: vec![
                FileDiff {
                    path: "src/auth.py".into(),
                    old_path: None,
                    status: FileStatus::Modified,
                    binary: false,
                    old_text: None,
                    new_text: Some("one\nTWO\nthree\n".into()),
                    hunks: vec![Hunk {
                        id: HunkId("h1".into()),
                        old_start: 1,
                        old_lines: 3,
                        new_start: 1,
                        new_lines: 3,
                        context: String::new(),
                        lines: vec![
                            diff_line(LineKind::Context, Some(1), Some(1), "one"),
                            diff_line(LineKind::Deleted, Some(2), None, "two"),
                            diff_line(LineKind::Added, None, Some(2), "TWO"),
                            diff_line(LineKind::Context, Some(3), Some(3), "three"),
                        ],
                    }],
                    hashes: diffler_core::model::HashCache::default(),
                },
                FileDiff {
                    path: "logo.png".into(),
                    old_path: None,
                    status: FileStatus::Added,
                    binary: true,
                    old_text: None,
                    new_text: None,
                    hunks: vec![],
                    hashes: diffler_core::model::HashCache::default(),
                },
            ],
        }
    }

    fn anchor(file: &str, line: Option<u32>) -> Anchor {
        Anchor {
            file: file.to_owned(),
            line,
            line_end: None,
            on_old_side: false,
            line_text: None,
        }
    }

    #[test]
    fn render_unified_emits_headers_hunks_and_origins() {
        let text = render_unified(&sample_model(), None).unwrap();
        assert!(text.contains("--- a/src/auth.py\n+++ b/src/auth.py\n"));
        assert!(text.contains("@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"));
        assert!(text.contains("--- /dev/null\n+++ b/logo.png\nBinary files differ\n"));
    }

    #[test]
    fn render_unified_filters_to_one_file() {
        let text = render_unified(&sample_model(), Some("src/auth.py")).unwrap();
        assert!(text.contains("src/auth.py"));
        assert!(!text.contains("logo.png"));
    }

    #[test]
    fn render_unified_unknown_file_is_an_error() {
        let err = render_unified(&sample_model(), Some("nope.rs")).unwrap_err();
        assert!(err.contains("nope.rs"));
    }

    #[test]
    fn render_unified_marks_deleted_files_with_dev_null() {
        let mut model = sample_model();
        model.files[0].status = FileStatus::Deleted;
        let text = render_unified(&model, Some("src/auth.py")).unwrap();
        assert!(text.contains("--- a/src/auth.py\n+++ /dev/null\n"));
    }

    #[test]
    fn render_unified_renamed_file_pairs_old_and_new_paths() {
        let mut model = sample_model();
        model.files[0].status = FileStatus::Renamed;
        model.files[0].old_path = Some("src/auth_v1.py".to_owned());
        let text = render_unified(&model, Some("src/auth.py")).unwrap();
        assert!(
            text.contains("--- a/src/auth_v1.py\n+++ b/src/auth.py\n"),
            "rename header must pair both paths: {text}"
        );
    }

    #[test]
    fn render_unified_empty_model_is_an_empty_diff() {
        let model = DiffModel { files: vec![] };
        assert_eq!(render_unified(&model, None).unwrap(), "");
    }

    #[test]
    fn comment_info_old_side_anchor_reports_side_and_context() {
        let mut session = Session::default();
        let mut a = anchor("src/auth.py", Some(2));
        a.on_old_side = true;
        a.line_text = Some("two".to_owned());
        session.add_comment(a, "human", "what was wrong with two?");
        let info = comment_info(
            &session.comments[0],
            &sample_model(),
            &ReviewSource::WorkingTree,
        );
        assert_eq!(info.side, "old");
        assert_eq!(info.line, Some(2));
        assert_eq!(info.line_end, None);
        assert!(!info.outdated, "the deleted line is still in the diff");
        assert_eq!(info.context.as_deref(), Some(" one\n-two\n+TWO"));
    }

    #[test]
    fn comment_info_carries_context_and_thread() {
        let mut session = Session::default();
        let mut a = anchor("src/auth.py", Some(2));
        a.line_text = Some("TWO".to_owned());
        let id = session.add_comment(a, "human", "why uppercase?").id.clone();
        session.reply(&id, AGENT_AUTHOR, "legacy API");
        let info = comment_info(
            &session.comments[0],
            &sample_model(),
            &ReviewSource::WorkingTree,
        );
        assert_eq!(info.file, "src/auth.py");
        assert_eq!(info.line, Some(2));
        assert_eq!(info.side, "new");
        assert_eq!(info.status, "replied");
        assert_eq!(info.context.as_deref(), Some("-two\n+TWO\n three"));
        assert!(!info.outdated);
        assert_eq!(info.replies.len(), 1);
        assert_eq!(info.replies[0].author, AGENT_AUTHOR);
    }

    #[test]
    fn comment_info_flags_departed_lines_outdated() {
        let mut session = Session::default();
        session.add_comment(anchor("src/auth.py", Some(99)), "human", "moved on");
        let info = comment_info(
            &session.comments[0],
            &sample_model(),
            &ReviewSource::WorkingTree,
        );
        assert!(info.outdated);
        assert_eq!(info.context, None);
    }

    #[test]
    fn comment_info_flags_drifted_line_text_outdated() {
        let mut session = Session::default();
        let mut a = anchor("src/auth.py", Some(2));
        a.line_text = Some("old text".to_owned());
        session.add_comment(a, "human", "stale");
        let info = comment_info(
            &session.comments[0],
            &sample_model(),
            &ReviewSource::WorkingTree,
        );
        assert!(info.outdated);
        assert!(info.context.is_some(), "context still renders");
    }

    #[test]
    fn file_level_comment_outdated_only_when_file_left_the_diff() {
        let mut session = Session::default();
        session.add_comment(anchor("src/auth.py", None), "human", "overall");
        session.add_comment(anchor("gone.py", None), "human", "gone");
        let model = sample_model();
        assert!(!comment_info(&session.comments[0], &model, &ReviewSource::WorkingTree).outdated);
        assert!(comment_info(&session.comments[1], &model, &ReviewSource::WorkingTree).outdated);
    }

    // A range comment where start != end is NOT outdated when the end
    // line still matches.  Only the end-line text is checked for drift; the
    // context snippet is still rooted at the start line.
    #[test]
    fn range_comment_not_outdated_when_end_line_matches() {
        let mut session = Session::default();
        // comment spans new lines 1-3; line_text snapshots line 3 ("three")
        let mut a = Anchor {
            file: "src/auth.py".to_owned(),
            line: Some(1),
            line_end: Some(3),
            on_old_side: false,
            line_text: Some("three".to_owned()),
        };
        // sanity: start text differs from end text
        assert_ne!(a.line_text.as_deref(), Some("one"));
        session.add_comment(a.clone(), "human", "range comment");
        let info = comment_info(
            &session.comments[0],
            &sample_model(),
            &ReviewSource::WorkingTree,
        );
        assert!(
            !info.outdated,
            "end line text matches snapshot: must NOT be outdated"
        );
        // change the snapshot to something that no longer matches line 3
        a.line_text = Some("changed".to_owned());
        session.comments[0].anchor = a;
        let info2 = comment_info(
            &session.comments[0],
            &sample_model(),
            &ReviewSource::WorkingTree,
        );
        assert!(info2.outdated, "end line text drifted: must be outdated");
    }

    // request_with_timeout returns a "busy" error when the reply
    // channel is never answered within the deadline.
    #[tokio::test]
    async fn request_times_out_when_app_does_not_answer() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (_feedback_tx, feedback_rx) = tokio::sync::watch::channel(0u64);
        let handler = DifflerMcp::new(tx, feedback_rx);
        let err = handler
            .request_with_timeout(McpRequestKind::ReviewStatus, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("busy"),
            "error should mention busy: {err:?}"
        );
    }

    // The long-poll cap must hold even when the caller asks for more.
    // Under tokio's paused clock the runtime auto-advances to the next
    // timer, so the elapsed virtual time is exactly the effective timeout.
    #[tokio::test(start_paused = true)]
    async fn wait_for_feedback_clamps_the_timeout_to_the_cap() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (_feedback_tx, feedback_rx) = tokio::sync::watch::channel(0u64);
        let handler = DifflerMcp::new(tx, feedback_rx);
        let started = tokio::time::Instant::now();
        let Json(response) = handler
            .wait_for_feedback(Parameters(WaitForFeedbackParams {
                since_epoch: None,
                timeout_seconds: Some(MAX_WAIT_SECONDS * 10),
            }))
            .await
            .unwrap();
        assert!(response.timed_out, "no feedback ever arrives");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(MAX_WAIT_SECONDS),
            "a timeout above the cap must clamp to {MAX_WAIT_SECONDS}s"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_feedback_defaults_inside_the_client_request_timeout() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (_feedback_tx, feedback_rx) = tokio::sync::watch::channel(0u64);
        let handler = DifflerMcp::new(tx, feedback_rx);
        let started = tokio::time::Instant::now();
        let Json(response) = handler
            .wait_for_feedback(Parameters(WaitForFeedbackParams {
                since_epoch: None,
                timeout_seconds: None,
            }))
            .await
            .unwrap();
        assert!(response.timed_out, "no feedback ever arrives");
        assert!(
            started.elapsed() + REQUEST_TIMEOUT < Duration::from_secs(120),
            "the wait plus the reply it still owes must beat the client's 120 s cut"
        );
    }

    // The cap is chosen so even the worst case (a full wait, then the app
    // round trip it still owes) stays under Claude Code's 120 s request cut.
    #[test]
    fn a_capped_wait_and_its_round_trip_fit_the_client_ceiling() {
        assert!(MAX_WAIT_SECONDS + REQUEST_TIMEOUT.as_secs() < 120);
    }

    #[tokio::test]
    async fn wait_for_feedback_answers_with_the_reviews_comments() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (feedback_tx, feedback_rx) = tokio::sync::watch::channel(0u64);
        feedback_tx.send_modify(|epoch| *epoch += 1);
        let handler = DifflerMcp::new(tx, feedback_rx);
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = requests.clone();
        tokio::spawn(async move {
            while let Some(AppEvent::Mcp(request)) = rx.recv().await {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let response = match request.kind {
                    McpRequestKind::Feedback => McpResponse::Feedback {
                        comments: Vec::new(),
                    },
                    _ => McpResponse::Ok,
                };
                let _ = request.reply.send(response);
            }
        });
        let Json(response) = handler
            .wait_for_feedback(Parameters(WaitForFeedbackParams {
                since_epoch: Some(0),
                timeout_seconds: None,
            }))
            .await
            .unwrap();
        assert!(!response.timed_out);
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the success path makes exactly one app request after the wait"
        );
    }

    // `DIFFLER_STATE_DIR` is process-global, so every test that touches the
    // registry serializes on this lock and restores the previous value,
    // keeping unrelated tests from reading a half-set env var mid-mutation.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[allow(unsafe_code)]
    fn with_state_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serializes every test that reads or writes this var.
        unsafe { std::env::set_var("DIFFLER_STATE_DIR", dir) };
        let result = f();
        unsafe { std::env::remove_var("DIFFLER_STATE_DIR") };
        result
    }

    #[test]
    fn endpoint_file_publishes_the_port_and_clears() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = tempfile::tempdir().expect("tempdir");
        with_state_dir(state.path(), || {
            write_endpoint(dir.path(), 8417).expect("write");
            let path = dir.path().join(".diffler/mcp.json");
            let body = std::fs::read_to_string(&path).expect("read");
            assert!(body.contains("\"port\": 8417"), "{body}");
            assert!(body.contains("http://127.0.0.1:8417/mcp"), "{body}");
            assert!(body.contains("\"pid\""), "{body}");
            // the .diffler dir self-gitignores like the session store
            let gitignore =
                std::fs::read_to_string(dir.path().join(".diffler/.gitignore")).expect("gitignore");
            assert_eq!(gitignore, "*\n");
            clear_endpoint(dir.path(), 8417);
            assert!(!path.exists(), "endpoint file removed on shutdown");
        });
    }

    #[test]
    fn clear_endpoint_leaves_a_newer_owner_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = tempfile::tempdir().expect("tempdir");
        with_state_dir(state.path(), || {
            write_endpoint(dir.path(), 1111).expect("write first instance");
            // a second diffler instance in the same repo overwrites the file
            write_endpoint(dir.path(), 2222).expect("write second instance");

            // the first instance shuts down and clears its own (stale) port
            clear_endpoint(dir.path(), 1111);

            let path = dir.path().join(".diffler/mcp.json");
            let body = std::fs::read_to_string(&path)
                .expect("file survives: it names the still-running instance");
            assert!(body.contains("\"port\": 2222"), "{body}");
        });
    }

    #[test]
    fn write_endpoint_registers_the_instance_for_cross_repo_discovery() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = tempfile::tempdir().expect("tempdir");
        with_state_dir(state.path(), || {
            write_endpoint(dir.path(), 8417).expect("write");
            let entry_path = state.path().join("diffler/instances/8417.json");
            let body = std::fs::read_to_string(&entry_path).expect("registry entry written");
            // the entry is JSON, and a Windows path's separators are escaped in
            // it, so the fields are read rather than matched as substrings
            let entry: serde_json::Value =
                serde_json::from_str(&body).expect("registry entry is json");
            let repo = dir.path().canonicalize().expect("canonicalize");
            assert_eq!(entry["repo"].as_str(), repo.to_str(), "{body}");
            assert_eq!(entry["port"].as_u64(), Some(8417), "{body}");
            assert_eq!(
                entry["pid"].as_u64(),
                Some(u64::from(std::process::id())),
                "{body}"
            );
            assert_eq!(
                entry["url"].as_str(),
                Some("http://127.0.0.1:8417/mcp"),
                "{body}"
            );
        });
    }

    #[test]
    fn clear_endpoint_removes_only_its_own_registry_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = tempfile::tempdir().expect("tempdir");
        with_state_dir(state.path(), || {
            write_endpoint(dir.path(), 1111).expect("write first instance");
            write_endpoint(dir.path(), 2222).expect("write second instance");
            let instances = state.path().join("diffler/instances");

            clear_endpoint(dir.path(), 1111);

            assert!(
                !instances.join("1111.json").exists(),
                "the cleared instance's own registry entry is gone"
            );
            assert!(
                instances.join("2222.json").exists(),
                "a different instance's registry entry survives"
            );
        });
    }

    // Rust's integer formats ("uint", "uint32", "uint64") are not registered
    // JSON Schema formats, and strict clients warn on every one they see.
    #[test]
    fn tool_schemas_use_no_unregistered_format() {
        for tool in DifflerMcp::tool_router().list_all() {
            for (part, schema) in [
                ("input", Some(&*tool.input_schema)),
                ("output", tool.output_schema.as_deref()),
            ] {
                let Some(schema) = schema else { continue };
                let text = serde_json::to_string(schema).expect("schema serializes");
                assert!(
                    !text.contains("\"format\""),
                    "{} {part} schema carries a format keyword: {text}",
                    tool.name
                );
            }
        }
    }

    // spawn_mcp falls back to :0 only on AddrInUse, not on other errors.
    #[tokio::test]
    async fn spawn_mcp_fallback_on_addr_in_use() {
        // grab a port and hold the listener so the configured port is busy
        let holder = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let busy_port = holder.local_addr().unwrap().port();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (_feedback_tx, feedback_rx) = tokio::sync::watch::channel(0u64);
        let handle = spawn_mcp(tx, feedback_rx, busy_port).unwrap();
        assert_ne!(
            handle.port, busy_port,
            "should have bound to a different ephemeral port"
        );
        handle.handle.abort();
    }
}

#[cfg(test)]
mod agent_command_sync {
    /// The review loop is written out for two agent harnesses. Only the
    /// frontmatter differs (each host wants its own shape), so the numbered
    /// steps have to stay identical or one harness quietly teaches a stale
    /// loop. A wording fix that lands in one and not the other is invisible
    /// without this.
    /// A Windows checkout carries CRLF, so the newlines are normalised before
    /// the frontmatter is found and the bodies are compared. Matching on `\n`
    /// alone silently falls through to comparing the frontmatter, which does
    /// differ, and fails on that platform only.
    fn body(doc: &str) -> String {
        let doc = doc.replace("\r\n", "\n");
        let after_frontmatter = doc
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---\n"))
            .map_or(doc.as_str(), |(_, body)| body);
        after_frontmatter.trim().to_owned()
    }

    /// A Windows checkout carries CRLF, where matching the frontmatter on `\n`
    /// alone finds none and ships the YAML header to the agent as prose.
    #[test]
    fn a_skill_loses_its_frontmatter_whatever_its_line_endings() {
        assert_eq!(
            super::skill_body("---\r\nname: df\r\n---\r\n\r\nthe body\r\n"),
            "the body"
        );
        assert_eq!(
            super::skill_body("---\nname: df\n---\n\nthe body\n"),
            "the body"
        );
    }

    /// Every agent command ships twice, as a Claude Code skill and an `OpenCode`
    /// command, and the two must say the same thing.
    #[test]
    fn both_agent_command_files_teach_the_same_loop() {
        const DF_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/df/SKILL.md"
        ));
        const DF_OPENCODE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.opencode/commands/df.md"
        ));
        const DFA_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/dfa/SKILL.md"
        ));
        const DFA_OPENCODE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.opencode/commands/dfa.md"
        ));
        const DFR_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/dfr/SKILL.md"
        ));
        const DFR_OPENCODE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.opencode/commands/dfr.md"
        ));
        for (name, skill, opencode, prompt) in [
            ("df", DF_SKILL, DF_OPENCODE, super::DF_SKILL),
            ("dfa", DFA_SKILL, DFA_OPENCODE, super::DFA_SKILL),
            ("dfr", DFR_SKILL, DFR_OPENCODE, super::DFR_SKILL),
        ] {
            assert_eq!(
                body(skill),
                body(opencode),
                "skills/{name}/SKILL.md and .opencode/commands/{name}.md have drifted; \
                 edit both or neither"
            );
            assert_eq!(
                body(skill),
                body(prompt),
                "skills/{name}/SKILL.md and crates/diffler/prompts/{name}.md have drifted; \
                 the crate ships its own copy because a published crate carries only \
                 its own directory"
            );
        }
    }

    /// The section from a `## Write for the card` heading to the next `## `
    /// heading or the end, trimmed. Each skill keeps its own steps and its
    /// own reply/stop-specific bullets above this heading, but the writing
    /// rules under it (voice, identifiers, tables) are shared verbatim.
    fn write_for_the_card(doc: &str) -> &str {
        const HEADING: &str = "## Write for the card";
        let Some(start) = doc.find(HEADING) else {
            return "";
        };
        let after = &doc[start..];
        let end = after[HEADING.len()..]
            .find("\n## ")
            .map_or(after.len(), |offset| offset + HEADING.len());
        after[..end].trim()
    }

    /// Answering a comment, writing a stop, and writing a review comment are
    /// all a card in the same pane, so all three skills must teach the same
    /// rules for it: first person plural, identifiers in backticks, plain
    /// verbs and the no-metaphor list, and tables for comparisons.
    #[test]
    fn every_skill_writes_the_card_the_same_way() {
        const DF_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/df/SKILL.md"
        ));
        const DFA_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/dfa/SKILL.md"
        ));
        const DFR_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/dfr/SKILL.md"
        ));
        let df = write_for_the_card(DF_SKILL);
        let dfa = write_for_the_card(DFA_SKILL);
        let dfr = write_for_the_card(DFR_SKILL);
        assert!(
            !df.is_empty(),
            "df must have a '## Write for the card' section"
        );
        assert_eq!(
            df, dfa,
            "the '## Write for the card' section must be identical in df and dfa"
        );
        assert_eq!(
            df, dfr,
            "the '## Write for the card' section must be identical in df and dfr"
        );
    }

    /// The `review`, `walkthrough` and `critique` MCP prompts are generated
    /// from `skills/df/SKILL.md`, `skills/dfa/SKILL.md` and
    /// `skills/dfr/SKILL.md`, so this checks the wiring rather than the
    /// wording: the prompt an agent receives over MCP must be that file's
    /// numbered steps, verbatim.
    #[tokio::test]
    async fn the_prompts_are_their_skills_bodies() {
        const DF_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/df/SKILL.md"
        ));
        const DFA_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/dfa/SKILL.md"
        ));
        const DFR_SKILL: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../skills/dfr/SKILL.md"
        ));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (_feedback_tx, feedback_rx) = tokio::sync::watch::channel(0u64);
        let handler = super::DifflerMcp::new(tx, feedback_rx);
        for (messages, skill) in [
            (handler.review().await, DF_SKILL),
            (handler.walkthrough().await, DFA_SKILL),
            (handler.critique().await, DFR_SKILL),
        ] {
            let text = &messages[0]
                .content
                .as_text()
                .expect("prompt carries text")
                .text;
            assert_eq!(*text, body(skill));
        }
    }
}
