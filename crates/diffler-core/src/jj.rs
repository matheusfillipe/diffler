//! jj backend for the [`Vcs`] trait: a colocated jj/git repo (`.jj` beside
//! `.git`, as `jj git init --colocate` makes). Reads delegate to [`GitVcs`],
//! since a colocated repo keeps git's HEAD on jj's `@-` and the working
//! copy on disk matches `@`, so diff, log, blame, and tree reads already
//! answer correctly through git2. Writes shell out to the `jj` CLI: jj owns
//! the operation log and working-copy snapshot, and duplicating that through
//! git2 would fight the real source of truth.
//!
//! jj has no index, so there is nothing to stage: [`Vcs::status`] puts the
//! whole working copy in one section instead of splitting it three ways.
//! Staging, unstaging, hunk staging, and stash have no jj equivalent and
//! are refused.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::git::GitVcs;
use crate::model::{DiffModel, HunkId};
use crate::vcs::{
    BlameSpan, BranchInfo, HeadInfo, LogEntry, NetworkOp, StatusModel, Vcs, VcsError, VcsKind,
};

pub struct JjVcs {
    git: GitVcs,
    root: PathBuf,
}

impl JjVcs {
    pub fn open(root: &Path) -> Result<Self, VcsError> {
        Self::open_with_context(root, crate::git::DEFAULT_CONTEXT_LINES)
    }

    pub fn open_with_context(root: &Path, context_lines: u32) -> Result<Self, VcsError> {
        Ok(Self {
            git: GitVcs::open_with_context(root, context_lines)?,
            root: root.to_path_buf(),
        })
    }

    /// Run a jj subcommand with the repo root as its working directory (jj
    /// resolves paths against the process cwd, not `-R`) and return trimmed
    /// stdout. A non-zero exit becomes a [`VcsError::Rejected`] carrying jj's
    /// first line of stderr, which is always the `Error: ...` summary.
    fn run(&self, args: &[&str]) -> Result<String, VcsError> {
        let output = Command::new("jj")
            .current_dir(&self.root)
            .args(args)
            .output()
            .map_err(|_| {
                VcsError::Rejected("jj not found on PATH; install jujutsu to use this repo".into())
            })?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("jj command failed")
            .trim()
            .to_owned();
        Err(VcsError::Rejected(message))
    }

    /// The oid a write just landed on: colocation exports jj's state to git
    /// synchronously, so HEAD (which sits on `@-`) already reflects it.
    fn head_oid(&self) -> Result<String, VcsError> {
        self.git.resolve("HEAD")
    }
}

const NO_STAGING: &str =
    "jj tracks the working copy directly; commit it with c c instead of staging";
const NO_UNSTAGING: &str = "jj has no staging area to unstage from";
const NO_STASH: &str = "jj has no stash; jj new starts a fresh change without losing this one";

impl Vcs for JjVcs {
    fn vcs_kind(&self) -> VcsKind {
        VcsKind::Jj
    }

    fn git_dir(&self) -> Result<PathBuf, VcsError> {
        self.git.git_dir()
    }

    fn head(&self) -> Result<HeadInfo, VcsError> {
        self.git.head()
    }

    /// Built from `working_tree_diff` (`@-` vs the whole working copy in one
    /// diff pass), not from merging git's own untracked/unstaged/staged
    /// lists: jj's snapshot marks a new file intent-to-add in the colocated
    /// index, which git2 then reports once as added (tree vs index) and
    /// again as modified (index vs workdir), double-counting it.
    fn status(&self) -> Result<StatusModel, VcsError> {
        Ok(StatusModel {
            untracked: DiffModel::default(),
            unstaged: DiffModel::default(),
            staged: self.git.working_tree_diff()?,
        })
    }

    fn working_tree_diff(&self) -> Result<DiffModel, VcsError> {
        self.git.working_tree_diff()
    }

    fn tree_to_workdir_diff(&self, base_oid: &str) -> Result<DiffModel, VcsError> {
        self.git.tree_to_workdir_diff(base_oid)
    }

    fn commit_diff(&self, oid: &str) -> Result<DiffModel, VcsError> {
        self.git.commit_diff(oid)
    }

    fn range_diff(&self, oldest_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError> {
        self.git.range_diff(oldest_oid, newest_oid)
    }

    fn tree_diff(&self, base_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError> {
        self.git.tree_diff(base_oid, newest_oid)
    }

    fn merge_base(&self, a: &str, b: &str) -> Result<String, VcsError> {
        self.git.merge_base(a, b)
    }

    fn resolve(&self, revision: &str) -> Result<String, VcsError> {
        self.git.resolve(revision)
    }

    fn log(&self, limit: usize) -> Result<Vec<LogEntry>, VcsError> {
        self.git.log(limit)
    }

    fn default_branch(&self, remote: &str) -> Result<Option<String>, VcsError> {
        self.git.default_branch(remote)
    }

    fn commits_between(&self, base: &str, head: &str) -> Result<Vec<LogEntry>, VcsError> {
        self.git.commits_between(base, head)
    }

    fn unpushed(&self, limit: usize) -> Result<Option<Vec<LogEntry>>, VcsError> {
        self.git.unpushed(limit)
    }

    fn blame(&self, rel: &Path) -> Result<Vec<BlameSpan>, VcsError> {
        self.git.blame(rel)
    }

    fn read_at(&self, rev: &str, path: &str) -> Result<Option<String>, VcsError> {
        self.git.read_at(rev, path)
    }

    fn tracked_files(&self) -> Result<Vec<PathBuf>, VcsError> {
        self.git.tracked_files()
    }

    fn attr(&self, rel: &Path, name: &str) -> bool {
        self.git.attr(rel, name)
    }

    fn branches(&self) -> Result<Vec<BranchInfo>, VcsError> {
        self.git.branches()
    }

    fn divergence(&self, branch: &str) -> Result<Option<(usize, usize)>, VcsError> {
        self.git.divergence(branch)
    }

    fn all_branches(&self) -> Result<Vec<String>, VcsError> {
        self.git.all_branches()
    }

    fn stage(&self, _rel: &Path) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn stage_everything(&self) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn unstage_everything(&self) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_UNSTAGING.into()))
    }

    fn stage_hunk(&self, _rel: &Path, _hunk: &HunkId) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STAGING.into()))
    }

    fn unstage(&self, _rel: &Path) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_UNSTAGING.into()))
    }

    fn unstage_hunk(&self, _rel: &Path, _hunk: &HunkId) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_UNSTAGING.into()))
    }

    fn discard(&self, rel: &Path) -> Result<(), VcsError> {
        self.run(&["restore", &rel.to_string_lossy()])?;
        Ok(())
    }

    fn commit(&self, message: &str) -> Result<String, VcsError> {
        if message.trim().is_empty() {
            return Err(VcsError::Rejected("empty commit message".into()));
        }
        self.run(&["commit", "-m", message])?;
        self.head_oid()
    }

    fn head_message(&self) -> Result<String, VcsError> {
        self.git.head_message()
    }

    /// `message: None` folds the working copy into its parent, keeping the
    /// parent's description (`jj squash`, jj's own "extend"); `Some` with
    /// `use_index` folds and sets the new description in the same squash;
    /// `Some` without `use_index` reword the parent alone (`jj describe`),
    /// leaving the working copy untouched.
    fn amend(&self, message: Option<&str>, use_index: bool) -> Result<String, VcsError> {
        if let Some(message) = message
            && message.trim().is_empty()
        {
            return Err(VcsError::Rejected("empty commit message".into()));
        }
        match message {
            None => self.run(&["squash"])?,
            Some(message) if use_index => self.run(&["squash", "-m", message])?,
            Some(message) => self.run(&["describe", "-r", "@-", "-m", message])?,
        };
        self.head_oid()
    }

    /// Labels the current change (`@`) with a bookmark; `checkout` is a
    /// no-op since `@` is already where the reader is standing.
    fn create_branch(&self, name: &str, _checkout: bool) -> Result<(), VcsError> {
        self.run(&["bookmark", "create", name, "-r", "@"])?;
        Ok(())
    }

    fn delete_branch(&self, name: &str) -> Result<(), VcsError> {
        self.run(&["bookmark", "delete", name])?;
        Ok(())
    }

    /// `jj new <rev>` rather than `jj edit`: checkout means keep working on
    /// top of the branch, not edit its tip commit in place, and `jj new`
    /// never discards the change being left behind, only detaches it.
    fn checkout(&self, name: &str) -> Result<(), VcsError> {
        self.run(&["new", name])?;
        Ok(())
    }

    fn stash_push(&self, _message: Option<&str>) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STASH.into()))
    }

    fn stash_pop(&self) -> Result<(), VcsError> {
        Err(VcsError::Rejected(NO_STASH.into()))
    }

    fn network_argv(&self, op: NetworkOp) -> Vec<String> {
        let args: &[&str] = match op {
            NetworkOp::Fetch => &["git", "fetch"],
            NetworkOp::FetchAll => &["git", "fetch", "--all-remotes"],
        };
        std::iter::once("jj")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    fn workdir(&self) -> Result<PathBuf, VcsError> {
        self.git.workdir()
    }

    fn remote_url(&self, name: &str) -> Result<Option<String>, VcsError> {
        self.git.remote_url(name)
    }

    fn remotes(&self) -> Result<Vec<String>, VcsError> {
        self.git.remotes()
    }
}
