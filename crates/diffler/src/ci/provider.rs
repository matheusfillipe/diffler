//! The forge seam: one async trait per forge adapter covering CI acquisition
//! and pull-request review. The host holds a `Box<dyn ForgeProvider + Send>`
//! chosen at runtime by detection.

use async_trait::async_trait;

use crate::ci::error::{CiError, Result};
use crate::ci::model::{
    Capabilities, CiRun, JobId, LogChunk, PrComment, PullRequest, RunDetail, RunExtras, RunId,
};

/// Which forge an adapter talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    GitHub,
    GitLab,
    Forgejo,
}

// `Sync` (not just `Send`) is required so the async-trait macro can generate a
// boxed future for the PR-review methods' default bodies below without
// knowing the concrete `Self` — every adapter is already `Sync` (their fields
// are plain owned data plus a `Box<dyn CommandRunner>`, itself `Send + Sync`).
#[async_trait]
pub trait ForgeProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    /// What this provider can do, so the UI degrades honestly.
    fn capabilities(&self) -> Capabilities;

    /// The most recent runs, newest first, capped at `limit`.
    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>>;

    /// A run's jobs and dependency edges.
    async fn run_detail(&self, run: &RunId) -> Result<RunDetail>;

    /// A slice of a job's log starting at `offset`. For `LogMode::Dump`
    /// providers the whole log returns once the job completes.
    async fn job_log(&self, run: &RunId, job: &JobId, offset: u64) -> Result<LogChunk>;

    /// A run's artifacts and annotations, for the graph page's extras panel.
    /// A provider that exposes neither returns the empty default.
    async fn run_extras(&self, run: &RunId) -> Result<RunExtras>;

    /// The pull/merge request for the checked-out branch, if one is open.
    /// `None` when there's no PR or the provider can't resolve one.
    async fn current_pr(&self) -> Result<Option<PullRequest>>;

    /// The repo's open pull/merge requests, newest first.
    async fn list_prs(&self) -> Result<Vec<PullRequest>>;

    /// Line-anchored review comments on a PR; empty when the provider has no
    /// review API. Default: no review API, so no comments.
    async fn pr_comments(&self, _number: u64) -> Result<Vec<PrComment>> {
        Ok(Vec::new())
    }

    /// Post a new line comment on the PR head, returning the forge's record.
    /// Default: unsupported.
    async fn post_pr_comment(&self, _new: &NewPrComment) -> Result<PrComment> {
        Err(CiError::Unsupported("posting PR comments"))
    }

    /// Reply to an existing PR review comment thread. Default: unsupported.
    async fn reply_pr_comment(
        &self,
        _number: u64,
        _remote_id: &str,
        _body: &str,
    ) -> Result<PrComment> {
        Err(CiError::Unsupported("replying to PR comments"))
    }

    /// Submit a batch of line comments as one review, so the forge sends a
    /// single notification instead of one per comment. Default: unsupported.
    async fn submit_pr_review(&self, _review: &NewPrReview) -> Result<()> {
        Err(CiError::Unsupported("submitting PR reviews"))
    }

    /// Resolve or unresolve a review thread. `thread_id` is the forge's
    /// thread handle carried on the root comment from `pr_comments`.
    /// Default: unsupported.
    async fn resolve_pr_thread(
        &self,
        _number: u64,
        _thread_id: &str,
        _resolved: bool,
    ) -> Result<()> {
        Err(CiError::Unsupported("resolving PR threads"))
    }

    /// Rewrite the body of one of our own review comments. Default: unsupported.
    async fn update_pr_comment(&self, _remote_id: &str, _body: &str) -> Result<()> {
        Err(CiError::Unsupported("editing PR comments"))
    }

    /// Delete one of our own review comments. Default: unsupported.
    async fn delete_pr_comment(&self, _remote_id: &str) -> Result<()> {
        Err(CiError::Unsupported("deleting PR comments"))
    }

    /// The PR as the forge sees it right now, for spotting a force-push
    /// while the review is open. Default: unsupported.
    async fn pr(&self, _number: u64) -> Result<PullRequest> {
        Err(CiError::Unsupported("PR lookup"))
    }
}

/// The event a submitted review carries on the forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Approve,
    RequestChanges,
    Comment,
}

/// A whole review to submit at once: the verdict, an optional top-level
/// body, and every pending line comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPrReview {
    pub number: u64,
    pub head_oid: String,
    pub verdict: ReviewVerdict,
    /// Shown above the line comments on the forge; empty means none.
    pub body: String,
    pub comments: Vec<NewPrComment>,
}

/// A comment to post, anchored to a diff line of `path` at `head_oid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPrComment {
    pub number: u64,
    pub head_oid: String,
    pub path: String,
    /// 1-based line on the side the comment anchors to; for a multi-line
    /// comment this is the range's last line.
    pub line: u32,
    /// First line of a multi-line comment; `None` anchors a single line.
    pub start_line: Option<u32>,
    /// Anchored to the new side (`RIGHT`) or the old side of the diff.
    pub new_side: bool,
    pub body: String,
}
