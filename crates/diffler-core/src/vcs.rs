//! Backend-agnostic VCS interface. Only the `git` and `repo` modules and test
//! fixtures may import git2.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::diffalgo::DiffAlgorithm;
use crate::model::{DiffModel, HunkId};

#[derive(Debug, Error)]
pub enum VcsError {
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
    /// Commits on HEAD the upstream lacks. Zero without an upstream.
    pub ahead: usize,
    /// Commits on the upstream that HEAD lacks.
    pub behind: usize,
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
    pub tip_unix: i64,
    /// `(ahead, behind)` against the upstream. Resolving one costs a graph
    /// walk, so listings leave it `None` and callers ask
    /// [`Vcs::divergence`] for the branches they show.
    pub divergence: Option<(usize, usize)>,
}

/// A network operation we run through the backend's CLI, so the user's own
/// auth applies and diffler holds no credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkOp {
    Fetch,
    FetchAll,
    Push,
    PushSetUpstream { remote: String },
    Pull,
    PullFrom { remote: String, branch: String },
    PullRebase,
    PullMerge,
}

/// A run of consecutive lines a single commit last touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameSpan {
    /// 1-based first line of the run in the blamed content.
    pub start_line: u32,
    pub line_count: u32,
    pub oid: String,
    pub oid7: String,
    pub author: String,
    pub time_unix: i64,
    pub summary: String,
    /// False for lines that exist only in the worktree, which no commit owns.
    pub committed: bool,
}

/// Per-area views of the working tree, neogit-style sections.
#[derive(Debug, Clone, Default)]
pub struct StatusModel {
    pub untracked: DiffModel,
    pub unstaged: DiffModel,
    pub staged: DiffModel,
}

pub trait Vcs: Send {
    /// Resolved metadata directory. In a linked worktree this is the gitdir
    /// the `.git` file points at.
    fn git_dir(&self) -> Result<PathBuf, VcsError>;
    fn head(&self) -> Result<HeadInfo, VcsError>;
    /// Whether this backend has a staging area. jj has none, so its whole
    /// working copy reads as [`StatusModel::staged`].
    fn has_index(&self) -> bool;
    fn status(&self) -> Result<StatusModel, VcsError>;
    /// HEAD vs index + worktree, untracked files included.
    fn working_tree_diff(&self) -> Result<DiffModel, VcsError>;
    /// [`Vcs::working_tree_diff`] taken from `base_oid`.
    fn tree_to_workdir_diff(&self, base_oid: &str) -> Result<DiffModel, VcsError>;
    /// Changes a single commit introduced over its first parent.
    fn commit_diff(&self, oid: &str) -> Result<DiffModel, VcsError>;
    /// `git diff <oldest>^..<newest>`. A root `oldest` diffs from the empty
    /// tree.
    fn range_diff(&self, oldest_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError>;
    /// `git diff <base> <newest>`; with `base` a merge base this is a
    /// three-dot diff.
    fn tree_diff(&self, base_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError>;
    fn merge_base(&self, a: &str, b: &str) -> Result<String, VcsError>;
    /// Resolve a revision (oid, ref name, remote ref) to a full commit oid.
    fn resolve(&self, revision: &str) -> Result<String, VcsError>;
    /// History from HEAD, newest first.
    fn log(&self, limit: usize) -> Result<Vec<LogEntry>, VcsError>;

    /// The branch a pull request merges into by default, if the repo says.
    fn default_branch(&self, remote: &str) -> Result<Option<String>, VcsError>;

    /// Commits reachable from `head` but not `base`, newest first.
    fn commits_between(&self, base: &str, head: &str) -> Result<Vec<LogEntry>, VcsError>;
    /// Commits on HEAD that no remote-tracking branch contains, newest first,
    /// at most `limit`. `None` when the repo has no remote-tracking refs. We
    /// ask the remotes since a configured upstream may be a local branch or a
    /// stale ref.
    fn unpushed(&self, limit: usize) -> Result<Option<Vec<LogEntry>>, VcsError>;
    /// Last commit to touch each line of `rel` as the worktree has it, in line
    /// order. Uncommitted lines come back as spans owned by no commit.
    fn blame(&self, rel: &Path) -> Result<Vec<BlameSpan>, VcsError>;

    /// One file's content in `rev`'s tree, `None` when the tree lacks it.
    fn read_at(&self, rev: &str, path: &str) -> Result<Option<String>, VcsError>;

    /// A blob's raw bytes by its hex id (a [`crate::model::BlobIds`] side),
    /// `None` when the object store has no such blob.
    fn read_blob(&self, oid: &str) -> Result<Option<Vec<u8>>, VcsError>;

    /// Every tracked file, repo-relative and sorted. This is the index, so a
    /// staged new file is tracked and an untracked one is not.
    fn tracked_files(&self) -> Result<Vec<PathBuf>, VcsError>;

    /// Whether the repo's git attributes set `name` to true for `rel`.
    /// Unreadable attribute files read as unset.
    fn attr(&self, rel: &Path, name: &str) -> bool;

    /// Local branches, their divergence left unresolved.
    fn branches(&self) -> Result<Vec<BranchInfo>, VcsError>;

    /// `(ahead, behind)` against the upstream, `None` when `branch` tracks
    /// nothing.
    fn divergence(&self, branch: &str) -> Result<Option<(usize, usize)>, VcsError>;
    /// Local and remote-tracking branch names.
    fn all_branches(&self) -> Result<Vec<String>, VcsError>;
    /// Stage a whole file (worktree deletions become staged deletions).
    fn stage(&self, rel: &Path) -> Result<(), VcsError>;

    /// Stage every change in the worktree, deletions and untracked files
    /// included.
    fn stage_everything(&self) -> Result<(), VcsError>;

    /// Reset the whole index back to HEAD, keeping the worktree.
    fn unstage_everything(&self) -> Result<(), VcsError>;
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
    /// index into the new tree; false keeps HEAD's tree (a pure reword).
    fn amend(&self, message: Option<&str>, use_index: bool) -> Result<String, VcsError>;
    fn create_branch(&self, name: &str, checkout: bool) -> Result<(), VcsError>;
    /// Refused for the currently checked-out branch.
    fn delete_branch(&self, name: &str) -> Result<(), VcsError>;
    fn checkout(&self, name: &str) -> Result<(), VcsError>;
    /// Whether a raw `git`/forge CLI command may check out a branch. False for
    /// jj, whose operation log would miss the write, so we go through
    /// [`Vcs::checkout`] there.
    fn native_git_checkout(&self) -> bool;
    /// Stash tracked changes, leaving untracked files in place like
    /// `git stash`. `message` `None` lets the backend label it.
    fn stash_push(&self, message: Option<&str>) -> Result<(), VcsError>;
    /// Restore the most recent stash and drop it. Refused when there is no
    /// stash or the pop would conflict.
    fn stash_pop(&self) -> Result<(), VcsError>;
    /// Argv for a network op, run in [`Vcs::workdir`] so the backend's own CLI
    /// handles credentials. jj rejects push and pull, since they would move
    /// git's refs behind its back.
    fn network_argv(&self, op: NetworkOp) -> Result<Vec<String>, VcsError>;
    fn workdir(&self) -> Result<PathBuf, VcsError>;
    fn remote_url(&self, name: &str) -> Result<Option<String>, VcsError>;
    fn remotes(&self) -> Result<Vec<String>, VcsError>;

    /// Set the line-diff algorithm for every later diff of this instance.
    fn set_diff_algorithm(&self, algorithm: DiffAlgorithm, indent_heuristic: bool);
}

/// Three-dot diff of `rev` against the working tree: `merge-base(rev, HEAD)`
/// vs index + worktree + untracked. Commits `rev` gained since the branch
/// forked stay out of it; uncommitted work stays in.
pub fn against_diff(vcs: &dyn Vcs, rev: &str) -> Result<DiffModel, VcsError> {
    let head = vcs.resolve("HEAD")?;
    let base = vcs.merge_base(&vcs.resolve(rev)?, &head)?;
    vcs.tree_to_workdir_diff(&base)
}
