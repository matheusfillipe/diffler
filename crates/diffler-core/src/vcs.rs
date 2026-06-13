//! Backend-agnostic VCS interface. Everything above this trait consumes
//! `dyn Vcs`; only the `git` module (and test fixtures) may import git2.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::model::{DiffModel, HunkId};

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
    /// Resolved repository metadata directory. In a plain repo this is
    /// `<root>/.git`; in a linked worktree `<root>/.git` is a gitlink file
    /// and this resolves to the external gitdir it points at.
    fn git_dir(&self) -> Result<PathBuf, VcsError>;
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
    /// Stage a whole file (worktree deletions become staged deletions).
    fn stage(&self, rel: &Path) -> Result<(), VcsError>;
    /// Stage one hunk out of the unstaged (or untracked) changes of a file.
    fn stage_hunk(&self, rel: &Path, hunk: &HunkId) -> Result<(), VcsError>;
    /// Reset a file's index entry back to HEAD, keeping the worktree.
    fn unstage(&self, rel: &Path) -> Result<(), VcsError>;
    /// Remove one staged hunk from the index, keeping the worktree.
    fn unstage_hunk(&self, rel: &Path, hunk: &HunkId) -> Result<(), VcsError>;
    /// Throw away worktree changes only; an untracked file is deleted.
    /// Refused while the file has staged changes (unstage first).
    fn discard(&self, rel: &Path) -> Result<(), VcsError>;
    /// Commit the index; returns the new commit id.
    fn commit(&self, message: &str) -> Result<String, VcsError>;
    fn create_branch(&self, name: &str, checkout: bool) -> Result<(), VcsError>;
    /// Refused for the currently checked-out branch.
    fn delete_branch(&self, name: &str) -> Result<(), VcsError>;
    fn checkout(&self, name: &str) -> Result<(), VcsError>;
}
