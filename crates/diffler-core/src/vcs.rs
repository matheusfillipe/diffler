//! Backend-agnostic VCS interface. Everything above this trait consumes
//! `dyn Vcs`; only the `git` module (and test fixtures) may import git2.

use thiserror::Error;

use crate::model::DiffModel;

#[derive(Debug, Error)]
pub enum VcsError {
    // acceptable for M1; rework when a second backend lands
    #[error(transparent)]
    Git(#[from] git2::Error),
    #[error("repository has no working directory")]
    NoWorkdir,
    /// Domain refusal, e.g. discarding a file with staged changes.
    #[error("{0}")]
    Rejected(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadInfo {
    /// Branch shorthand; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Abbreviated commit id; empty on an unborn branch.
    pub oid7: String,
    /// First line of the HEAD commit message; empty on an unborn branch.
    pub subject: String,
    /// Upstream branch shorthand, if configured.
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub oid: String,
    pub oid7: String,
    /// Shorthand names of references pointing at this commit.
    pub refs: Vec<String>,
    pub subject: String,
    pub author: String,
    pub time_unix: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
}

/// Per-area views of the working tree, neogit-style sections.
#[derive(Debug, Clone, Default)]
pub struct StatusModel {
    pub untracked: DiffModel,
    pub unstaged: DiffModel,
    pub staged: DiffModel,
}

pub trait Vcs: Send {
    /// Current branch, commit, and upstream.
    fn head(&self) -> Result<HeadInfo, VcsError>;
    /// Untracked / unstaged / staged sections as separate diff models.
    fn status(&self) -> Result<StatusModel, VcsError>;
    /// HEAD vs workdir+index including untracked files: the review view.
    fn working_tree_diff(&self) -> Result<DiffModel, VcsError>;
    /// Changes a single commit introduced over its first parent.
    fn commit_diff(&self, oid: &str) -> Result<DiffModel, VcsError>;
    /// History from HEAD, newest first.
    fn log(&self, limit: usize) -> Result<Vec<LogEntry>, VcsError>;
    /// Local branches.
    fn branches(&self) -> Result<Vec<BranchInfo>, VcsError>;
}
