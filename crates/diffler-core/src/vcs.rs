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

/// A network operation the binary runs by shelling out to the backend's CLI,
/// so the user's existing auth (SSH agent, credential helper, tokens) applies
/// without diffler holding any credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkOp {
    Push,
    PushSetUpstream,
    Pull,
    Fetch,
    FetchAll,
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
    /// Combined diff of a contiguous commit range, from the first parent of
    /// `oldest` to `newest` (`git diff <oldest>^..<newest>` semantics). When
    /// `oldest` is a root commit its first-parent tree is the empty tree, so
    /// the range includes everything `oldest` introduced.
    fn range_diff(&self, oldest_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError>;
    /// Diff between two trees as-is (`git diff <base> <newest>` semantics);
    /// with `base` a merge base this is a PR-style three-dot diff.
    fn tree_diff(&self, base_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError>;
    /// Best common ancestor of two commits.
    fn merge_base(&self, a: &str, b: &str) -> Result<String, VcsError>;
    /// Resolve a revision (oid, ref name, remote ref) to a full commit oid.
    fn resolve(&self, revision: &str) -> Result<String, VcsError>;
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
    /// Full message of the HEAD commit, for amend/reword editor templates.
    fn head_message(&self) -> Result<String, VcsError>;
    /// Amend HEAD, returning the new commit id. `message` `None` reuses HEAD's
    /// message (extend); `Some` rewords it. `use_index` true folds the staged
    /// index into the new tree (extend/amend); false keeps HEAD's tree (a
    /// pure reword). Local-only — no network.
    fn amend(&self, message: Option<&str>, use_index: bool) -> Result<String, VcsError>;
    fn create_branch(&self, name: &str, checkout: bool) -> Result<(), VcsError>;
    /// Refused for the currently checked-out branch.
    fn delete_branch(&self, name: &str) -> Result<(), VcsError>;
    fn checkout(&self, name: &str) -> Result<(), VcsError>;
    /// Stash tracked changes (staged + unstaged), reverting the worktree to
    /// HEAD; untracked files are left in place, matching `git stash`. `message`
    /// `None` lets the backend label it. Local-only — no network.
    fn stash_push(&self, message: Option<&str>) -> Result<(), VcsError>;
    /// Restore the most recent stash and drop it. Refused when there is no
    /// stash or the pop would conflict.
    fn stash_pop(&self) -> Result<(), VcsError>;
    /// Argv to run for a network op, e.g. `["git", "push"]`. The binary runs
    /// this in [`Vcs::workdir`] so the backend's own CLI handles credentials;
    /// diffler never touches them. A future jj backend returns `["jj", …]`.
    fn network_argv(&self, op: NetworkOp) -> Vec<String>;
    /// Working directory to run [`Vcs::network_argv`] in.
    fn workdir(&self) -> Result<PathBuf, VcsError>;
    /// URL of the named remote (e.g. `origin`), if it exists. Used to detect the
    /// CI provider's host without shelling out.
    fn remote_url(&self, name: &str) -> Result<Option<String>, VcsError>;
    /// Names of every configured remote, for multi-remote CI detection.
    fn remotes(&self) -> Result<Vec<String>, VcsError>;
}
