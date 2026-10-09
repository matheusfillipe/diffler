//! The forge seam: one async trait covering CI acquisition and pull-request review.

use async_trait::async_trait;

use crate::ci::error::{CiError, Result};
use crate::ci::model::{
    Capabilities, CiRun, DagSource, JobId, LogChunk, LogMode, PrComment, PullRequest, RunDetail,
    RunExtras, RunId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    GitHub,
    GitLab,
    Forgejo,
}

impl ProviderKind {
    /// The ref a plain `git fetch` names to get a pull request's head.
    pub fn pr_head_ref(self, number: u64) -> String {
        match self {
            ProviderKind::GitLab => format!("refs/merge-requests/{number}/head"),
            ProviderKind::GitHub | ProviderKind::Forgejo => format!("refs/pull/{number}/head"),
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ProviderKind::GitHub => "GitHub",
            ProviderKind::GitLab => "GitLab",
            ProviderKind::Forgejo => "Forgejo",
        })
    }
}

/// Standalone so the UI can gate an affordance on a detected kind without
/// building a provider.
pub fn capabilities_for(kind: ProviderKind) -> Capabilities {
    match kind {
        ProviderKind::GitHub => Capabilities {
            dag: DagSource::ConfigFile,
            logs: LogMode::Dump,
            resolve_threads: true,
            file_comments: true,
        },
        ProviderKind::GitLab => Capabilities {
            dag: DagSource::RunApi,
            logs: LogMode::Poll,
            resolve_threads: true,
            file_comments: true,
        },
        ProviderKind::Forgejo => Capabilities {
            dag: DagSource::None,
            logs: LogMode::None,
            resolve_threads: false,
            file_comments: false,
        },
    }
}

// async-trait needs `Sync` to box the default methods' futures without knowing `Self`
#[async_trait]
pub trait ForgeProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    fn capabilities(&self) -> Capabilities {
        capabilities_for(self.kind())
    }

    /// Newest first.
    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>>;

    async fn run_detail(&self, run: &RunId) -> Result<RunDetail>;

    /// For `LogMode::Dump` providers the whole log arrives once the job completes.
    async fn job_log(&self, run: &RunId, job: &JobId, offset: u64) -> Result<LogChunk>;

    async fn run_extras(&self, run: &RunId) -> Result<RunExtras>;

    /// The open PR for the checked-out branch.
    async fn current_pr(&self) -> Result<Option<PullRequest>>;

    /// Newest first.
    async fn list_prs(&self) -> Result<Vec<PullRequest>>;

    async fn pr_comments(&self, _number: u64) -> Result<Vec<PrComment>> {
        Ok(Vec::new())
    }

    async fn post_pr_comment(&self, _new: &NewPrComment) -> Result<PrComment> {
        Err(CiError::Unsupported("posting PR comments"))
    }

    async fn reply_pr_comment(
        &self,
        _number: u64,
        _remote_id: &str,
        _body: &str,
    ) -> Result<PrComment> {
        Err(CiError::Unsupported("replying to PR comments"))
    }

    /// One review, so the forge sends a single notification.
    async fn submit_pr_review(&self, _review: &NewPrReview) -> Result<()> {
        Err(CiError::Unsupported("submitting PR reviews"))
    }

    /// `thread_id` comes from the root comment's `PrComment::thread_id`.
    async fn resolve_pr_thread(
        &self,
        _number: u64,
        _thread_id: &str,
        _resolved: bool,
    ) -> Result<()> {
        Err(CiError::Unsupported("resolving PR threads"))
    }

    /// `number` is there because a forge can scope the edit route to the PR.
    async fn update_pr_comment(&self, _number: u64, _remote_id: &str, _body: &str) -> Result<()> {
        Err(CiError::Unsupported("editing PR comments"))
    }

    /// `number` is there because a forge can scope the delete route to the PR.
    async fn delete_pr_comment(&self, _number: u64, _remote_id: &str) -> Result<()> {
        Err(CiError::Unsupported("deleting PR comments"))
    }

    /// The PR as the forge sees it now, for spotting a force-push mid-review.
    async fn pr(&self, _number: u64) -> Result<PullRequest> {
        Err(CiError::Unsupported("PR lookup"))
    }

    async fn create_pr(&self, _new: &NewPullRequest) -> Result<PullRequest> {
        Err(CiError::Unsupported("opening a pull request"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPullRequest {
    pub base: String,
    pub head: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPrReview {
    pub number: u64,
    pub head_oid: String,
    pub verdict: ReviewVerdict,
    /// Empty means none.
    pub body: String,
    pub comments: Vec<NewPrComment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPrComment {
    pub number: u64,
    pub head_oid: String,
    pub path: String,
    /// 1-based, the last line of a range; `None` anchors the whole file.
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub new_side: bool,
    /// The same row's number on the other side, set only for a line both sides
    /// share. GitLab rejects an unchanged line named from one side alone.
    pub counterpart: Option<u32>,
    pub body: String,
}
