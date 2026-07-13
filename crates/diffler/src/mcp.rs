//! Embedded MCP server: agents read review comments, answer them in place,
//! and long-poll for the human's feedback. Tool handlers never touch the
//! review directly — every call is sent through the app event channel as an
//! [`McpRequest`] and answered by `App::handle_mcp` on the main loop, so the
//! app stays the single owner of all state (no locks).

use std::fmt;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use diffler_core::feedback;
use diffler_core::model::{DiffModel, FileStatus};
use diffler_core::session::{Comment, CommentStatus};
use diffler_core::source::ReviewSource;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{PromptMessage, Role, ServerCapabilities, ServerInfo};
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

const DEFAULT_WAIT_SECONDS: u64 = 300;
const MAX_WAIT_SECONDS: u64 = 540;
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
    /// Open + replied comments for `wait_for_feedback` after an epoch bump.
    Feedback,
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
    /// Domain refusal (unknown id/file): surfaces as a tool error.
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReviewStatusResponse {
    pub repo: String,
    pub branch: Option<String>,
    pub oid7: String,
    pub files_changed: Vec<FileEntry>,
    pub open_comments: usize,
    pub replied_comments: usize,
    pub resolved_comments: usize,
    pub feedback_epoch: u64,
    /// Every persisted review and its comment counts; `files_changed` above is
    /// the working-tree review, the default the human starts on.
    pub reviews: Vec<ReviewSummary>,
}

/// One review's provenance and comment tally: what was reviewed (working tree,
/// a commit, or a range) and how many comments sit at each status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ReviewSummary {
    /// Stable source key (e.g. "working", "commit-<oid>", "range-<a>-<b>").
    pub source: String,
    /// Human-facing description (e.g. "commit a1b2c3", "range a1b2c3..d4e5f6").
    pub label: String,
    pub open_comments: usize,
    pub replied_comments: usize,
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
    pub line: Option<u32>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct WaitForFeedbackResponse {
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
pub struct WaitForFeedbackParams {
    /// Return once the feedback epoch exceeds this value. Defaults to the
    /// current epoch, i.e. wait for the next human send.
    pub since_epoch: Option<u64>,
    /// Long-poll timeout (default 300, max 540).
    pub timeout_seconds: Option<u64>,
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
                "diffler is busy (editor open or loop stalled) — retry",
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
        description = "List every review the human has — the working tree, individual commits, and commit ranges — each with its comment counts, so you can tell where feedback came from."
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
        description = "Propose a comment as resolved: marks it replied with an agent note. Only the human can resolve it, in the TUI."
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
        description = "Long-poll until the human sends feedback (comments, replies, or the send key). Returns the new epoch and all open/replied comments, or timed_out."
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
        let epoch = match waited {
            Ok(Ok(epoch)) => epoch,
            Ok(Err(_)) => {
                return Err(ErrorData::internal_error(
                    "the diffler TUI is not running",
                    None,
                ));
            }
            Err(_) => {
                let epoch = *rx.borrow();
                return Ok(Json(WaitForFeedbackResponse {
                    epoch,
                    timed_out: true,
                    comments: Vec::new(),
                }));
            }
        };
        match self.request(McpRequestKind::Feedback).await? {
            McpResponse::Comments(comments) => Ok(Json(WaitForFeedbackResponse {
                epoch,
                timed_out: false,
                comments,
            })),
            _ => Err(mismatch()),
        }
    }
}

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
        vec![PromptMessage::new_text(
            Role::User,
            "Check the diffler review and respond to the human's feedback:\n\
             1. Call review_status for the active review and its changed files.\n\
             2. Call get_comments with status \"open\" and read each comment in place.\n\
             3. Address every comment in the code it anchors to.\n\
             4. Answer each with reply_comment (what you changed and why), then \
             propose_resolve; only the human can resolve for real, in the TUI.\n\
             5. Call wait_for_feedback with the latest epoch and start over when it \
             returns — the human just sent new feedback. If it times out, call it \
             again; if the connection fails, diffler is closed, so stop.",
        )]
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for DifflerMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_instructions(
            "diffler code review: get_comments reads the human's diff comments, \
             reply_comment answers them in place, propose_resolve flags them as \
             addressed, and wait_for_feedback long-polls until the human sends \
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

/// The `claude mcp add` hint shown in the status bar at startup.
pub fn connect_hint(port: u16) -> String {
    format!("mcp :{port} — claude mcp add --transport http diffler http://127.0.0.1:{port}/mcp")
}

/// Repo-relative path of the endpoint discovery file an external proxy reads.
const ENDPOINT_FILE: &str = "mcp.json";

fn endpoint_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join(".diffler").join(ENDPOINT_FILE)
}

/// Publish the live MCP endpoint to `.diffler/mcp.json` so a stdio proxy (the
/// `npx` bridge) can discover the actual port, which may differ from the
/// configured one after an ephemeral fallback.
pub fn write_endpoint(repo_root: &Path, port: u16) -> std::io::Result<()> {
    let dir = repo_root.join(".diffler");
    std::fs::create_dir_all(&dir)?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n")?;
    }
    let body = format!("{{\n  \"port\": {port},\n  \"url\": \"http://127.0.0.1:{port}/mcp\"\n}}\n");
    std::fs::write(endpoint_path(repo_root), body)
}

/// Remove the endpoint file on shutdown, but only when it still names this
/// process's own port. A second diffler instance in the same repo overwrites
/// the file with its own port; deleting unconditionally would let whichever
/// process exits first destroy the still-running one's proxy discovery.
pub fn clear_endpoint(repo_root: &Path, port: u16) {
    let path = endpoint_path(repo_root);
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    let current_owner = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("port").and_then(serde_json::Value::as_u64));
    if current_owner == Some(u64::from(port)) {
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
            "end line text matches snapshot — must NOT be outdated"
        );
        // change the snapshot to something that no longer matches line 3
        a.line_text = Some("changed".to_owned());
        session.comments[0].anchor = a;
        let info2 = comment_info(
            &session.comments[0],
            &sample_model(),
            &ReviewSource::WorkingTree,
        );
        assert!(info2.outdated, "end line text drifted — must be outdated");
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

    #[test]
    fn endpoint_file_publishes_the_port_and_clears() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_endpoint(dir.path(), 8417).expect("write");
        let path = dir.path().join(".diffler/mcp.json");
        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("\"port\": 8417"), "{body}");
        assert!(body.contains("http://127.0.0.1:8417/mcp"), "{body}");
        // the .diffler dir self-gitignores like the session store
        let gitignore =
            std::fs::read_to_string(dir.path().join(".diffler/.gitignore")).expect("gitignore");
        assert_eq!(gitignore, "*\n");
        clear_endpoint(dir.path(), 8417);
        assert!(!path.exists(), "endpoint file removed on shutdown");
    }

    #[test]
    fn clear_endpoint_leaves_a_newer_owner_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_endpoint(dir.path(), 1111).expect("write first instance");
        // a second diffler instance in the same repo overwrites the file
        write_endpoint(dir.path(), 2222).expect("write second instance");

        // the first instance shuts down and clears its own (stale) port
        clear_endpoint(dir.path(), 1111);

        let path = dir.path().join(".diffler/mcp.json");
        let body = std::fs::read_to_string(&path)
            .expect("file survives: it names the still-running instance");
        assert!(body.contains("\"port\": 2222"), "{body}");
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
