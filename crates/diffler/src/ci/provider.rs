//! The forge seam: one async trait per forge adapter covering CI acquisition
//! and pull-request review. The host holds a `Box<dyn ForgeProvider + Send>`
//! chosen at runtime by detection.

use async_trait::async_trait;

use crate::ci::error::Result;
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

#[async_trait]
pub trait ForgeProvider: Send {
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
    /// review API.
    async fn pr_comments(&self, number: u64) -> Result<Vec<PrComment>>;

    /// Post a new line comment on the PR head, returning the forge's record.
    async fn post_pr_comment(&self, new: &NewPrComment) -> Result<PrComment>;

    /// Reply to an existing PR review comment thread.
    async fn reply_pr_comment(&self, number: u64, remote_id: &str, body: &str)
    -> Result<PrComment>;
}

/// A comment to post, anchored to a diff line of `path` at `head_oid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPrComment {
    pub number: u64,
    pub head_oid: String,
    pub path: String,
    /// 1-based line on the side the comment anchors to.
    pub line: u32,
    /// Anchored to the new side (`RIGHT`) or the old side of the diff.
    pub new_side: bool,
    pub body: String,
}
