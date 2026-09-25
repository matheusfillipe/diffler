//! git2 backend for the [`Vcs`] trait: the only module that may touch git2
//! (test fixtures aside).

use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::diffalgo::{DiffAlgorithm, histogram_hunks};
use crate::model::{
    DiffLine, DiffModel, FileDiff, FileStatus, Hunk, HunkId, LineKind, disambiguated_hunk_id,
};
use crate::vcs::{
    BlameSpan, BranchInfo, HeadInfo, LogEntry, NetworkOp, StatusModel, Vcs, VcsError, VcsKind,
};

/// git's own default amount of context around hunks.
pub const DEFAULT_CONTEXT_LINES: u32 = 3;

/// git's own default: on, matching modern git's behaviour.
pub const DEFAULT_INDENT_HEURISTIC: bool = true;

pub struct GitVcs {
    repo: git2::Repository,
    context_lines: u32,
    /// The session's current line-diff algorithm. A `Cell` so a live palette
    /// switch (`Vcs::set_diff_algorithm`) reaches every diff this instance
    /// computes afterward without needing `&mut self`.
    algorithm: Cell<DiffAlgorithm>,
    indent_heuristic: Cell<bool>,
}

impl GitVcs {
    pub fn open(root: &Path) -> Result<Self, VcsError> {
        Self::open_with_context(root, DEFAULT_CONTEXT_LINES)
    }

    /// Open with a custom number of context lines around diff hunks, the
    /// default algorithm (myers) and indent heuristic on.
    pub fn open_with_context(root: &Path, context_lines: u32) -> Result<Self, VcsError> {
        Self::open_with_options(
            root,
            context_lines,
            DiffAlgorithm::default(),
            DEFAULT_INDENT_HEURISTIC,
        )
    }

    /// Open with a custom context, line-diff algorithm and indent heuristic
    /// (config keys `ui.context_lines`, `diff.algorithm`, `diff.indent_heuristic`).
    pub fn open_with_options(
        root: &Path,
        context_lines: u32,
        algorithm: DiffAlgorithm,
        indent_heuristic: bool,
    ) -> Result<Self, VcsError> {
        let repo = git2::Repository::open(root)?;
        if repo.workdir().is_none() {
            return Err(VcsError::NoWorkdir);
        }
        Ok(Self {
            repo,
            context_lines,
            algorithm: Cell::new(algorithm),
            indent_heuristic: Cell::new(indent_heuristic),
        })
    }

    /// HEAD tree, or `None` on an unborn branch (fresh repo).
    fn head_tree(&self) -> Result<Option<git2::Tree<'_>>, VcsError> {
        match self.repo.head() {
            Ok(head) => Ok(Some(head.peel_to_tree()?)),
            Err(err) if err.code() == git2::ErrorCode::UnbornBranch => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn workdir_path(&self) -> Result<&Path, VcsError> {
        self.repo.workdir().ok_or(VcsError::NoWorkdir)
    }

    /// `base` tree vs workdir+index including untracked, renames folded in.
    /// `None` is the empty tree (an unborn branch).
    fn workdir_diff(&self, base: Option<&git2::Tree<'_>>) -> Result<DiffModel, VcsError> {
        let mut diff = self
            .repo
            .diff_tree_to_workdir_with_index(base, Some(&mut self.workdir_diff_options()))?;
        let mut find = git2::DiffFindOptions::new();
        find.renames(true);
        diff.find_similar(Some(&mut find))?;
        self.diff_to_model(&mut diff)
    }

    /// `git2::DiffOptions` for a tree-vs-tree or tree-vs-index diff at the
    /// session's current context and algorithm.
    fn plain_diff_options(&self) -> git2::DiffOptions {
        let mut opts = git2::DiffOptions::new();
        opts.context_lines(self.context_lines);
        apply_git_algorithm(&mut opts, self.algorithm.get(), self.indent_heuristic.get());
        opts
    }

    /// Working-tree diff options (untracked files included) at the session's
    /// current context and algorithm.
    fn workdir_diff_options(&self) -> git2::DiffOptions {
        let mut opts = self.plain_diff_options();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        opts
    }

    /// Assemble a [`DiffModel`] from a computed git2 diff. `Myers`/`Minimal`/
    /// `Patience` already produced the right hunks (their flags were baked
    /// into the `DiffOptions` that built `diff`); `Histogram`/`Structural`
    /// have no git2 equivalent, so every non-binary file with both sides
    /// present gets its hunks re-derived through imara-diff instead. Rename
    /// and binary detection, resolved on `diff` itself, are untouched either
    /// way. Intra-line emphasis is a render-time concern: the TUI enriches
    /// the file it is about to draw (see `crate::pairing::enrich_file`), so
    /// every line here leaves `.emphasis` empty.
    fn diff_to_model(&self, diff: &mut git2::Diff<'_>) -> Result<DiffModel, VcsError> {
        let mut files = Vec::new();
        for idx in 0..diff.deltas().len() {
            if let Some(file) = build_file(&self.repo, diff, idx)? {
                files.push(file);
            }
        }
        let algorithm = self.algorithm.get();
        if algorithm.is_imara() {
            let indent_heuristic = self.indent_heuristic.get();
            for file in &mut files {
                if file.binary {
                    continue;
                }
                if let (Some(old), Some(new)) = (file.old_text.as_deref(), file.new_text.as_deref())
                {
                    file.hunks =
                        histogram_hunks(old, new, &file.path, self.context_lines, indent_heuristic);
                }
            }
        }
        Ok(DiffModel { files })
    }

    fn walk_entries(
        &self,
        walk: git2::Revwalk<'_>,
        limit: usize,
    ) -> Result<Vec<LogEntry>, VcsError> {
        let mut entries = Vec::new();
        for oid in walk.take(limit) {
            let oid = oid?;
            let commit = self.repo.find_commit(oid)?;
            let full = oid.to_string();
            entries.push(LogEntry {
                oid7: short7(&full),
                oid: full,
                refs: Vec::new(),
                subject: commit.summary()?.unwrap_or_default().to_owned(),
                author: commit.author().name().unwrap_or_default().to_owned(),
                time_unix: commit.time().seconds(),
            });
        }
        Ok(entries)
    }

    /// Whether any tracked file differs from HEAD or the index, i.e. there is
    /// something `git stash` would save. Untracked files don't count, matching
    /// stash's default.
    fn has_tracked_changes(&self) -> Result<bool, VcsError> {
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(false).include_ignored(false);
        let statuses = self.repo.statuses(Some(&mut opts))?;
        Ok(statuses.iter().next().is_some())
    }
}

impl Vcs for GitVcs {
    fn vcs_kind(&self) -> VcsKind {
        VcsKind::Git
    }

    fn git_dir(&self) -> Result<PathBuf, VcsError> {
        // libgit2 resolves gitlink files, so linked worktrees come back as
        // their external gitdir under the main repo's .git/worktrees/
        Ok(self.repo.path().to_path_buf())
    }

    fn head(&self) -> Result<HeadInfo, VcsError> {
        match self.repo.head() {
            Ok(head) => {
                let branch = if head.is_branch() {
                    Some(head.shorthand()?.to_owned())
                } else {
                    None
                };
                let commit = head.peel_to_commit()?;
                let tracking = branch.as_deref().and_then(|name| {
                    let local = self.repo.find_branch(name, git2::BranchType::Local).ok()?;
                    let upstream = local.upstream().ok()?;
                    let name = upstream.name().ok().flatten()?.to_owned();
                    Some((name, upstream.get().target()))
                });
                let (upstream, ahead, behind) = match tracking {
                    Some((name, Some(target))) => {
                        let (ahead, behind) = self
                            .repo
                            .graph_ahead_behind(commit.id(), target)
                            .unwrap_or((0, 0));
                        (Some(name), ahead, behind)
                    }
                    Some((name, None)) => (Some(name), 0, 0),
                    None => (None, 0, 0),
                };
                Ok(HeadInfo {
                    branch,
                    oid7: short7(&commit.id().to_string()),
                    subject: commit.summary()?.unwrap_or_default().to_owned(),
                    upstream,
                    ahead,
                    behind,
                })
            }
            Err(err) if err.code() == git2::ErrorCode::UnbornBranch => {
                let branch = self
                    .repo
                    .find_reference("HEAD")
                    .ok()
                    .and_then(|r| r.symbolic_target().ok().flatten().map(str::to_owned))
                    .and_then(|t| t.strip_prefix("refs/heads/").map(str::to_owned));
                Ok(HeadInfo {
                    branch,
                    oid7: String::new(),
                    subject: String::new(),
                    upstream: None,
                    ahead: 0,
                    behind: 0,
                })
            }
            Err(err) => Err(err.into()),
        }
    }

    fn status(&self) -> Result<StatusModel, VcsError> {
        // index vs workdir classifies "untracked" against the index, so a
        // staged new file lands in staged only, not here
        let mut workdir = self
            .repo
            .diff_index_to_workdir(None, Some(&mut self.workdir_diff_options()))?;
        let workdir_model = self.diff_to_model(&mut workdir)?;
        let (untracked, unstaged): (Vec<_>, Vec<_>) = workdir_model
            .files
            .into_iter()
            .partition(|f| f.status == FileStatus::Untracked);

        let head_tree = self.head_tree()?;
        let mut staged = self.repo.diff_tree_to_index(
            head_tree.as_ref(),
            None,
            Some(&mut self.plain_diff_options()),
        )?;
        let staged = self.diff_to_model(&mut staged)?;

        Ok(StatusModel {
            untracked: DiffModel { files: untracked },
            unstaged: DiffModel { files: unstaged },
            staged,
        })
    }

    fn working_tree_diff(&self) -> Result<DiffModel, VcsError> {
        self.workdir_diff(self.head_tree()?.as_ref())
    }

    fn tree_to_workdir_diff(&self, base_oid: &str) -> Result<DiffModel, VcsError> {
        let base = self.repo.find_commit(git2::Oid::from_str(base_oid)?)?;
        self.workdir_diff(Some(&base.tree()?))
    }

    fn commit_diff(&self, oid: &str) -> Result<DiffModel, VcsError> {
        let oid = git2::Oid::from_str(oid)?;
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;
        // root commit: first-parent tree is the empty tree
        let parent_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut diff = self.repo.diff_tree_to_tree(
            parent_tree.as_ref(),
            Some(&tree),
            Some(&mut self.plain_diff_options()),
        )?;
        self.diff_to_model(&mut diff)
    }

    fn tree_diff(&self, base_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError> {
        let base = self.repo.find_commit(git2::Oid::from_str(base_oid)?)?;
        let newest = self.repo.find_commit(git2::Oid::from_str(newest_oid)?)?;
        let mut diff = self.repo.diff_tree_to_tree(
            Some(&base.tree()?),
            Some(&newest.tree()?),
            Some(&mut self.plain_diff_options()),
        )?;
        self.diff_to_model(&mut diff)
    }

    fn merge_base(&self, a: &str, b: &str) -> Result<String, VcsError> {
        let base = self
            .repo
            .merge_base(git2::Oid::from_str(a)?, git2::Oid::from_str(b)?)?;
        Ok(base.to_string())
    }

    fn resolve(&self, revision: &str) -> Result<String, VcsError> {
        let object = self.repo.revparse_single(revision)?;
        let commit = object.peel_to_commit()?;
        Ok(commit.id().to_string())
    }

    fn range_diff(&self, oldest_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError> {
        let oldest = self.repo.find_commit(git2::Oid::from_str(oldest_oid)?)?;
        let newest = self.repo.find_commit(git2::Oid::from_str(newest_oid)?)?;
        let newest_tree = newest.tree()?;
        // the range starts before the oldest commit, so its base is that
        // commit's first parent; a root commit has none and diffs against the
        // empty tree, matching commit_diff
        let base_tree = oldest.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut diff = self.repo.diff_tree_to_tree(
            base_tree.as_ref(),
            Some(&newest_tree),
            Some(&mut self.plain_diff_options()),
        )?;
        self.diff_to_model(&mut diff)
    }

    fn log(&self, limit: usize) -> Result<Vec<LogEntry>, VcsError> {
        if self.head_tree()?.is_none() {
            return Ok(Vec::new());
        }
        let mut refs_by_oid: HashMap<git2::Oid, Vec<String>> = HashMap::new();
        for reference in self.repo.references()?.flatten() {
            let Ok(name) = reference.shorthand().map(str::to_owned) else {
                continue;
            };
            // peel through symbolic refs and annotated tags to the commit
            let Some(target) = reference.peel_to_commit().ok().map(|c| c.id()) else {
                continue;
            };
            refs_by_oid.entry(target).or_default().push(name);
        }

        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        walk.push_head()?;
        let mut entries = Vec::new();
        for oid in walk.take(limit) {
            let oid = oid?;
            let commit = self.repo.find_commit(oid)?;
            let full = oid.to_string();
            entries.push(LogEntry {
                oid7: short7(&full),
                oid: full,
                refs: refs_by_oid.get(&oid).cloned().unwrap_or_default(),
                subject: commit.summary()?.unwrap_or_default().to_owned(),
                author: commit.author().name().unwrap_or_default().to_owned(),
                time_unix: commit.time().seconds(),
            });
        }
        Ok(entries)
    }

    fn default_branch(&self, remote: &str) -> Result<Option<String>, VcsError> {
        // the remote's own HEAD is authoritative; it exists once the remote
        // has been cloned or fetched with `--set-head`
        let head_ref = format!("refs/remotes/{remote}/HEAD");
        let prefix = format!("refs/remotes/{remote}/");
        if let Ok(reference) = self.repo.find_reference(&head_ref)
            && let Ok(Some(target)) = reference.symbolic_target()
            // the whole remainder, so a branch named `release/2.x` survives
            && let Some(name) = target.strip_prefix(prefix.as_str())
        {
            return Ok(Some(name.to_owned()));
        }
        for name in ["main", "master"] {
            if self
                .repo
                .find_branch(name, git2::BranchType::Local)
                .or_else(|_| {
                    self.repo
                        .find_branch(&format!("{remote}/{name}"), git2::BranchType::Remote)
                })
                .is_ok()
            {
                return Ok(Some(name.to_owned()));
            }
        }
        Ok(None)
    }

    fn commits_between(&self, base: &str, head: &str) -> Result<Vec<LogEntry>, VcsError> {
        let (base, head) = (
            self.repo.revparse_single(base)?,
            self.repo.revparse_single(head)?,
        );
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        walk.push(head.id())?;
        walk.hide(base.id())?;
        self.walk_entries(walk, usize::MAX)
    }

    fn unpushed(&self, limit: usize) -> Result<Option<Vec<LogEntry>>, VcsError> {
        let Ok(head) = self.repo.head() else {
            return Ok(None);
        };
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        walk.push(head.peel_to_commit()?.id())?;
        let mut remotes = 0;
        for reference in self.repo.references()? {
            let reference = reference?;
            // a symbolic ref (origin/HEAD) has no target of its own and its
            // destination is hidden anyway
            if let (true, Some(oid)) = (reference.is_remote(), reference.target()) {
                remotes += 1;
                walk.hide(oid)?;
            }
        }
        if remotes == 0 {
            return Ok(None);
        }
        self.walk_entries(walk, limit).map(Some)
    }

    fn blame(&self, rel: &Path) -> Result<Vec<BlameSpan>, VcsError> {
        let mut options = git2::BlameOptions::new();
        options.track_copies_same_file(true);
        let blame = self.repo.blame_file(rel, Some(&mut options))?;
        // blame_file only knows committed content, so an edited worktree would
        // report the wrong line for everything below the edit; blame_buffer
        // re-maps the spans onto what is actually on disk.
        let workdir = self.workdir()?.join(rel);
        let blame = match fs::read(&workdir) {
            Ok(bytes) => blame.blame_buffer(&bytes)?,
            Err(_) => blame,
        };

        let mut out = Vec::new();
        for hunk in blame.iter() {
            let oid = hunk.final_commit_id();
            let commit = self.repo.find_commit(oid).ok();
            let full = oid.to_string();
            out.push(BlameSpan {
                start_line: u32::try_from(hunk.final_start_line()).unwrap_or(u32::MAX),
                line_count: u32::try_from(hunk.lines_in_hunk()).unwrap_or(u32::MAX),
                oid7: short7(&full),
                oid: full,
                author: commit
                    .as_ref()
                    .map(|c| c.author().name().unwrap_or_default().to_owned())
                    .unwrap_or_default(),
                time_unix: commit.as_ref().map_or(0, |c| c.time().seconds()),
                summary: commit
                    .as_ref()
                    .and_then(|c| c.summary().ok().flatten().map(str::to_owned))
                    .unwrap_or_default(),
                committed: !oid.is_zero(),
            });
        }
        out.sort_by_key(|span| span.start_line);
        Ok(out)
    }

    fn read_at(&self, rev: &str, path: &str) -> Result<Option<String>, VcsError> {
        let commit = self.repo.revparse_single(rev)?.peel_to_commit()?;
        match commit.tree()?.get_path(Path::new(path)) {
            Ok(entry) => Ok(blob_text(&self.repo, entry.id())),
            Err(err) if err.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn tracked_files(&self) -> Result<Vec<PathBuf>, VcsError> {
        let index = self.repo.index()?;
        let mut out: Vec<PathBuf> = index
            .iter()
            .filter_map(|entry| {
                std::str::from_utf8(&entry.path)
                    .ok()
                    .map(|path| PathBuf::from(path.to_owned()))
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    fn attr(&self, rel: &Path, name: &str) -> bool {
        let Ok(value) = self.repo.get_attr(rel, name, git2::AttrCheckFlags::empty()) else {
            return false;
        };
        // both spellings are in the wild: a bare `linguist-generated` sets
        // git's boolean, while the `=true` GitHub documents is a string value
        matches!(
            git2::AttrValue::from_string(value),
            git2::AttrValue::True | git2::AttrValue::String("true")
        )
    }

    fn branches(&self) -> Result<Vec<BranchInfo>, VcsError> {
        let mut out = Vec::new();
        for entry in self.repo.branches(Some(git2::BranchType::Local))? {
            let (branch, _) = entry?;
            let Some(name) = branch.name()?.map(str::to_owned) else {
                continue;
            };
            let tip_unix = branch
                .get()
                .peel_to_commit()
                .ok()
                .map_or(0, |commit| commit.time().seconds());
            out.push(BranchInfo {
                name,
                is_head: branch.is_head(),
                tip_unix,
                divergence: None,
            });
        }
        Ok(out)
    }

    fn divergence(&self, branch: &str) -> Result<Option<(usize, usize)>, VcsError> {
        let branch = self.repo.find_branch(branch, git2::BranchType::Local)?;
        let Some(tip) = branch.get().peel_to_commit().ok() else {
            return Ok(None);
        };
        let Some(target) = branch.upstream().ok().and_then(|up| up.get().target()) else {
            return Ok(None);
        };
        Ok(Some(self.repo.graph_ahead_behind(tip.id(), target)?))
    }

    fn all_branches(&self) -> Result<Vec<String>, VcsError> {
        let mut out = Vec::new();
        for entry in self.repo.branches(None)? {
            let (branch, _) = entry?;
            let Some(name) = branch.name()?.map(str::to_owned) else {
                continue;
            };
            // refs/remotes/<remote>/HEAD is a symbolic alias, not a branch
            if name.ends_with("/HEAD") {
                continue;
            }
            out.push(name);
        }
        Ok(out)
    }

    fn stage(&self, rel: &Path) -> Result<(), VcsError> {
        let root = self.workdir_path()?;
        let mut index = self.repo.index()?;
        if root.join(rel).exists() {
            index.add_path(rel)?;
        } else {
            index.remove_path(rel)?;
        }
        index.write()?;
        Ok(())
    }

    fn stage_hunk(&self, rel: &Path, hunk: &HunkId) -> Result<(), VcsError> {
        let diff = self
            .repo
            .diff_index_to_workdir(None, Some(&mut self.workdir_diff_options()))?;
        let patch = self.synthesize_patch(&diff, rel, hunk, false)?;
        let diff = git2::Diff::from_buffer(&patch)?;
        self.repo.apply(&diff, git2::ApplyLocation::Index, None)?;
        Ok(())
    }

    fn stage_everything(&self) -> Result<(), VcsError> {
        let mut index = self.repo.index()?;
        // update_all catches deletions and edits to files already tracked;
        // add_all then picks up whatever is untracked
        index.update_all(["*"], None)?;
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        Ok(())
    }

    fn unstage_everything(&self) -> Result<(), VcsError> {
        match self.repo.head() {
            Ok(head) => {
                let target = head.peel(git2::ObjectType::Commit)?;
                self.repo.reset_default(Some(&target), ["*"])?;
            }
            // unborn branch: nothing in HEAD to restore, so empty the index
            Err(err) if err.code() == git2::ErrorCode::UnbornBranch => {
                let mut index = self.repo.index()?;
                index.clear()?;
                index.write()?;
            }
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    fn unstage(&self, rel: &Path) -> Result<(), VcsError> {
        match self.repo.head() {
            Ok(head) => {
                let target = head.peel(git2::ObjectType::Commit)?;
                self.repo.reset_default(Some(&target), [rel])?;
            }
            // unborn branch: there is no HEAD entry to restore, drop from index
            Err(err) if err.code() == git2::ErrorCode::UnbornBranch => {
                let mut index = self.repo.index()?;
                index.remove_path(rel)?;
                index.write()?;
            }
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }

    fn unstage_hunk(&self, rel: &Path, hunk: &HunkId) -> Result<(), VcsError> {
        let head_tree = self.head_tree()?;
        let diff = self.repo.diff_tree_to_index(
            head_tree.as_ref(),
            None,
            Some(&mut self.plain_diff_options()),
        )?;
        let patch = self.synthesize_patch(&diff, rel, hunk, true)?;
        let diff = git2::Diff::from_buffer(&patch)?;
        self.repo.apply(&diff, git2::ApplyLocation::Index, None)?;
        Ok(())
    }

    fn discard(&self, rel: &Path) -> Result<(), VcsError> {
        let head_tree = self.head_tree()?;
        let mut opts = git2::DiffOptions::new();
        opts.pathspec(rel).disable_pathspec_match(true);
        let staged = self
            .repo
            .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?;
        if staged.deltas().len() > 0 {
            return Err(VcsError::Rejected(
                "file has staged changes; unstage first".into(),
            ));
        }
        let status = self.repo.status_file(rel)?;
        if status.contains(git2::Status::WT_NEW) {
            fs::remove_file(self.workdir_path()?.join(rel))?;
            return Ok(());
        }
        // refresh the index stat cache to match the file we just wrote. with
        // autocrlf the checkout smudges LF->CRLF, growing the file; leaving the
        // cached stat stale makes git report a phantom modification (size
        // mismatch defeats the racy-clean check) even though the content is
        // identical to HEAD. the file has no staged changes here, so the index
        // blob already equals HEAD and updating it only corrects the metadata.
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.path(rel).force().update_index(true);
        self.repo.checkout_head(Some(&mut checkout))?;
        Ok(())
    }

    fn commit(&self, message: &str) -> Result<String, VcsError> {
        if message.trim().is_empty() {
            return Err(VcsError::Rejected("empty commit message".into()));
        }
        let mut index = self.repo.index()?;
        let tree_id = index.write_tree()?;
        let tree = self.repo.find_tree(tree_id)?;
        let signature = self.repo.signature()?;
        let parent = match self.repo.head() {
            Ok(head) => Some(head.peel_to_commit()?),
            Err(err) if err.code() == git2::ErrorCode::UnbornBranch => None,
            Err(err) => return Err(err.into()),
        };
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        let oid = self.repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )?;
        Ok(oid.to_string())
    }

    fn head_message(&self) -> Result<String, VcsError> {
        let commit = self.repo.head()?.peel_to_commit()?;
        Ok(commit.message().unwrap_or_default().to_owned())
    }

    fn amend(&self, message: Option<&str>, use_index: bool) -> Result<String, VcsError> {
        if let Some(message) = message
            && message.trim().is_empty()
        {
            return Err(VcsError::Rejected("empty commit message".into()));
        }
        let head = self.repo.head()?.peel_to_commit()?;
        // extend/amend fold the staged index into the new tree; a pure reword
        // keeps HEAD's tree so only the message changes
        let tree = if use_index {
            let mut index = self.repo.index()?;
            let tree_id = index.write_tree()?;
            self.repo.find_tree(tree_id)?
        } else {
            head.tree()?
        };
        let oid = head.amend(Some("HEAD"), None, None, None, message, Some(&tree))?;
        Ok(oid.to_string())
    }

    fn create_branch(&self, name: &str, checkout: bool) -> Result<(), VcsError> {
        let head = self.repo.head()?.peel_to_commit()?;
        self.repo.branch(name, &head, false)?;
        if checkout {
            self.checkout(name)?;
        }
        Ok(())
    }

    fn delete_branch(&self, name: &str) -> Result<(), VcsError> {
        let mut branch = self.repo.find_branch(name, git2::BranchType::Local)?;
        if branch.is_head() {
            return Err(VcsError::Rejected(
                "cannot delete the checked-out branch".into(),
            ));
        }
        branch.delete()?;
        Ok(())
    }

    fn checkout(&self, name: &str) -> Result<(), VcsError> {
        let branch = self.repo.find_branch(name, git2::BranchType::Local)?;
        let target = branch.get().peel(git2::ObjectType::Commit)?;
        // safe (non-force) checkout: refuses to clobber local modifications
        self.repo.checkout_tree(&target, None)?;
        self.repo.set_head(&format!("refs/heads/{name}"))?;
        Ok(())
    }

    fn stash_push(&self, message: Option<&str>) -> Result<(), VcsError> {
        if !self.has_tracked_changes()? {
            return Err(VcsError::Rejected("nothing to stash".into()));
        }
        // git2 stash mutates the repo, but the trait is &self; a fresh handle on
        // the same workdir gives the &mut without threading mutability everywhere
        let mut repo = git2::Repository::open(self.workdir_path()?)?;
        let signature = repo.signature()?;
        repo.stash_save2(&signature, message, None)?;
        Ok(())
    }

    fn stash_pop(&self) -> Result<(), VcsError> {
        let mut repo = git2::Repository::open(self.workdir_path()?)?;
        match repo.stash_pop(0, None) {
            Ok(()) => Ok(()),
            Err(err) if err.code() == git2::ErrorCode::NotFound => {
                Err(VcsError::Rejected("no stash to pop".into()))
            }
            // a conflicting pop leaves the merge in the worktree and keeps the
            // stash entry; say so rather than surfacing a bare libgit2 error
            Err(err) if err.code() == git2::ErrorCode::Conflict => Err(VcsError::Rejected(
                "stash applied with conflicts; resolve them (the stash was kept)".into(),
            )),
            Err(err) => Err(err.into()),
        }
    }

    fn network_argv(&self, op: NetworkOp) -> Vec<String> {
        // shelling to `git` (not git2) so the user's credential helper, SSH
        // agent, and config drive auth
        let args: &[&str] = match op {
            NetworkOp::Fetch => &["fetch"],
            NetworkOp::FetchAll => &["fetch", "--all"],
        };
        std::iter::once("git")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    fn workdir(&self) -> Result<PathBuf, VcsError> {
        Ok(self.workdir_path()?.to_path_buf())
    }

    fn remote_url(&self, name: &str) -> Result<Option<String>, VcsError> {
        match self.repo.find_remote(name) {
            // url() errs only on a non-UTF-8 remote URL; treat that as no URL
            Ok(remote) => Ok(remote.url().ok().map(str::to_owned)),
            Err(err) if err.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn remotes(&self) -> Result<Vec<String>, VcsError> {
        let array = self.repo.remotes()?;
        let mut names = Vec::new();
        for i in 0..array.len() {
            if let Ok(Some(name)) = array.get(i) {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    fn set_diff_algorithm(&self, algorithm: DiffAlgorithm, indent_heuristic: bool) {
        self.algorithm.set(algorithm);
        self.indent_heuristic.set(indent_heuristic);
    }

    fn diff_algorithm(&self) -> (DiffAlgorithm, bool) {
        (self.algorithm.get(), self.indent_heuristic.get())
    }
}

/// `DiffOptions` flags for the git2-native algorithms (myers is git2's
/// default, so it sets nothing); `Histogram`/`Structural` have no git2 flag
/// and are handled entirely by [`GitVcs::diff_to_model`] instead.
fn apply_git_algorithm(
    opts: &mut git2::DiffOptions,
    algorithm: DiffAlgorithm,
    indent_heuristic: bool,
) {
    match algorithm {
        DiffAlgorithm::Minimal => {
            opts.minimal(true);
        }
        DiffAlgorithm::Patience => {
            opts.patience(true);
        }
        DiffAlgorithm::Myers | DiffAlgorithm::Histogram | DiffAlgorithm::Structural => {}
    }
    opts.indent_heuristic(indent_heuristic);
}

impl GitVcs {
    /// Render one hunk of `rel` as a unified patch libgit2 can apply to the
    /// index. `reverse` flips the patch so applying it undoes a staged hunk.
    /// Under `Histogram`/`Structural`, `diff`'s own git2-computed hunks don't
    /// match what the reviewer sees (libgit2 has no histogram algorithm), so
    /// a modified file's hunks are re-derived the same way the display model
    /// is (`histogram_hunks`) and the patch is built from that model instead;
    /// this is also what keeps the hunk id the reviewer picked findable.
    /// Every other case (an add/delete/binary file, or a git2-native
    /// algorithm) still renders straight from git2's own patch lines, so
    /// original line endings and missing-trailing-newline markers survive
    /// untouched.
    fn synthesize_patch(
        &self,
        diff: &git2::Diff<'_>,
        rel: &Path,
        target: &HunkId,
        reverse: bool,
    ) -> Result<Vec<u8>, VcsError> {
        let rel = rel.to_string_lossy();
        for idx in 0..diff.deltas().len() {
            let Some(delta) = diff.get_delta(idx) else {
                continue;
            };
            if delta.flags().is_binary() || delta_new_path(&delta) != rel {
                continue;
            }
            let algorithm = self.algorithm.get();
            let old_text = blob_text(&self.repo, delta.old_file().id());
            let new_text = new_side_text(&self.repo, &delta, &rel);
            if algorithm.is_imara()
                && let (Some(old), Some(new)) = (old_text.as_deref(), new_text.as_deref())
            {
                let hunks = histogram_hunks(
                    old,
                    new,
                    &rel,
                    self.context_lines,
                    self.indent_heuristic.get(),
                );
                let Some(hunk) = hunks.into_iter().find(|h| h.id == *target) else {
                    return Err(VcsError::Rejected("hunk not found (diff changed?)".into()));
                };
                return Ok(render_hunk_patch_from_model(
                    &hunk,
                    &rel,
                    delta.status(),
                    reverse,
                    old,
                    new,
                ));
            }
            let Some(patch) = git2::Patch::from_diff(diff, idx)? else {
                continue;
            };
            let mut seen = HashMap::new();
            for h in 0..patch.num_hunks() {
                let lines = hunk_model_lines(&patch, h)?;
                if disambiguated_hunk_id(&rel, &lines, &mut seen) == *target {
                    return render_hunk_patch(&patch, h, &rel, delta.status(), reverse);
                }
            }
            return Err(VcsError::Rejected("hunk not found (diff changed?)".into()));
        }
        Err(VcsError::Rejected("hunk not found (diff changed?)".into()))
    }
}

fn render_hunk_patch(
    patch: &git2::Patch<'_>,
    h: usize,
    rel: &str,
    status: git2::Delta,
    reverse: bool,
) -> Result<Vec<u8>, VcsError> {
    let added = matches!(status, git2::Delta::Added | git2::Delta::Untracked);
    let deleted = status == git2::Delta::Deleted;
    let mut out = Vec::new();
    out.extend_from_slice(format!("diff --git a/{rel} b/{rel}\n").as_bytes());
    // whole-file adds and deletes must keep that identity (with the sides
    // swapped under reverse) so applying creates or drops the index entry
    // instead of leaving an empty blob behind
    if (added && !reverse) || (deleted && reverse) {
        out.extend_from_slice(
            format!("new file mode 100644\n--- /dev/null\n+++ b/{rel}\n").as_bytes(),
        );
    } else if (deleted && !reverse) || (added && reverse) {
        out.extend_from_slice(
            format!("deleted file mode 100644\n--- a/{rel}\n+++ /dev/null\n").as_bytes(),
        );
    } else {
        out.extend_from_slice(format!("--- a/{rel}\n+++ b/{rel}\n").as_bytes());
    }
    let (hunk, line_count) = patch.hunk(h)?;
    let (old, new) = (
        (hunk.old_start(), hunk.old_lines()),
        (hunk.new_start(), hunk.new_lines()),
    );
    let ((minus_start, minus_lines), (plus_start, plus_lines)) =
        if reverse { (new, old) } else { (old, new) };
    out.extend_from_slice(
        format!("@@ -{minus_start},{minus_lines} +{plus_start},{plus_lines} @@\n").as_bytes(),
    );
    for l in 0..line_count {
        let line = patch.line_in_hunk(h, l)?;
        let origin = match (line.origin(), reverse) {
            (' ', _) => Some(b' '),
            ('+', false) | ('-', true) => Some(b'+'),
            ('-', false) | ('+', true) => Some(b'-'),
            // EOF-newline markers already carry the full "\ No newline at
            // end of file" text, including the newline that terminates the
            // preceding unterminated line, so they pass through unprefixed
            ('=' | '>' | '<', _) => None,
            _ => continue,
        };
        if let Some(origin) = origin {
            out.push(origin);
        }
        out.extend_from_slice(line.content());
    }
    Ok(out)
}

/// [`render_hunk_patch`]'s counterpart for a hunk imara-diff computed:
/// there is no `git2::Patch` to read lines from, so the patch text is built
/// straight from the model's `DiffLine`s, and the "no newline at end of
/// file" markers are derived from whether `old_text`/`new_text` themselves
/// end in a newline (unified-diff patch lines are always themselves
/// newline-terminated; the marker line is what signals the original had none).
fn render_hunk_patch_from_model(
    hunk: &Hunk,
    rel: &str,
    status: git2::Delta,
    reverse: bool,
    old_text: &str,
    new_text: &str,
) -> Vec<u8> {
    let added = matches!(status, git2::Delta::Added | git2::Delta::Untracked);
    let deleted = status == git2::Delta::Deleted;
    let mut out = Vec::new();
    out.extend_from_slice(format!("diff --git a/{rel} b/{rel}\n").as_bytes());
    if (added && !reverse) || (deleted && reverse) {
        out.extend_from_slice(
            format!("new file mode 100644\n--- /dev/null\n+++ b/{rel}\n").as_bytes(),
        );
    } else if (deleted && !reverse) || (added && reverse) {
        out.extend_from_slice(
            format!("deleted file mode 100644\n--- a/{rel}\n+++ /dev/null\n").as_bytes(),
        );
    } else {
        out.extend_from_slice(format!("--- a/{rel}\n+++ b/{rel}\n").as_bytes());
    }
    let old = (hunk.old_start, hunk.old_lines);
    let new = (hunk.new_start, hunk.new_lines);
    let ((minus_start, minus_lines), (plus_start, plus_lines)) =
        if reverse { (new, old) } else { (old, new) };
    out.extend_from_slice(
        format!("@@ -{minus_start},{minus_lines} +{plus_start},{plus_lines} @@\n").as_bytes(),
    );

    #[allow(clippy::cast_possible_truncation)]
    let old_total = old_text.lines().count() as u32;
    #[allow(clippy::cast_possible_truncation)]
    let new_total = new_text.lines().count() as u32;
    let old_no_eof_nl = !old_text.is_empty() && !old_text.ends_with('\n');
    let new_no_eof_nl = !new_text.is_empty() && !new_text.ends_with('\n');

    for line in &hunk.lines {
        let out_origin: u8 = match (line.kind, reverse) {
            (LineKind::Context, _) => b' ',
            (LineKind::Added, false) | (LineKind::Deleted, true) => b'+',
            (LineKind::Deleted, false) | (LineKind::Added, true) => b'-',
        };
        out.push(out_origin);
        out.extend_from_slice(line.text.as_bytes());
        out.push(b'\n');
        let old_last = old_total > 0 && line.old_no == Some(old_total);
        let new_last = new_total > 0 && line.new_no == Some(new_total);
        let no_newline = match line.kind {
            LineKind::Deleted => old_last && old_no_eof_nl,
            LineKind::Added => new_last && new_no_eof_nl,
            LineKind::Context => (old_last && old_no_eof_nl) || (new_last && new_no_eof_nl),
        };
        if no_newline {
            out.extend_from_slice(b"\\ No newline at end of file\n");
        }
    }
    out
}

fn short7(oid: &str) -> String {
    oid.get(..7).unwrap_or(oid).to_owned()
}

fn build_file(
    repo: &git2::Repository,
    diff: &mut git2::Diff<'_>,
    idx: usize,
) -> Result<Option<FileDiff>, VcsError> {
    let Some(patch) = git2::Patch::from_diff(diff, idx)? else {
        // binary or unreadable: fall back to delta metadata only
        return Ok(build_binary_file(diff, idx));
    };
    let delta = patch.delta();
    if delta.flags().is_binary() {
        return Ok(build_binary_file(diff, idx));
    }
    let file_path = delta_new_path(&delta);
    let status = map_status(delta.status());
    let old_path = if status == FileStatus::Renamed {
        delta
            .old_file()
            .path()
            .map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    };

    let old_text = blob_text(repo, delta.old_file().id());
    let new_text = new_side_text(repo, &delta, &file_path);

    let hunks = patch_hunks(&patch, &file_path)?;

    Ok(Some(FileDiff {
        path: file_path,
        old_path,
        status,
        binary: false,
        old_text,
        new_text,
        hunks,
        hashes: crate::model::HashCache::default(),
    }))
}

/// Re-diff a file's own old/new text at `context` lines of surrounding
/// context (`u32::MAX` for the whole file) and `algorithm`, yielding the
/// hunks the diff pane would show at that context. `None` for binary files or
/// when a side's text is absent, so the caller keeps its current hunks.
pub fn rehunk_file(
    file: &FileDiff,
    context: u32,
    algorithm: DiffAlgorithm,
    indent_heuristic: bool,
) -> Option<Vec<Hunk>> {
    if file.binary {
        return None;
    }
    let (old, new) = (file.old_text.as_deref()?, file.new_text.as_deref()?);
    if algorithm.is_imara() {
        return Some(histogram_hunks(
            old,
            new,
            &file.path,
            context,
            indent_heuristic,
        ));
    }
    let as_path = Path::new(&file.path);
    // libgit2's context math overflows on a huge value (the whole-file
    // sentinel u32::MAX), yielding zero context on some platforms; the line
    // count is enough to show the whole file and stays in range everywhere
    let cap = u32::try_from(old.lines().count().max(new.lines().count())).unwrap_or(u32::MAX);
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(context.min(cap));
    apply_git_algorithm(&mut opts, algorithm, indent_heuristic);
    let patch = git2::Patch::from_buffers(
        old.as_bytes(),
        Some(as_path),
        new.as_bytes(),
        Some(as_path),
        Some(&mut opts),
    )
    .ok()?;
    patch_hunks(&patch, &file.path).ok()
}

/// Assemble model hunks from a git2 patch. Shared by the initial diff and the
/// context re-diff so line numbers, ids, and section headings can't drift.
fn patch_hunks(patch: &git2::Patch<'_>, file_path: &str) -> Result<Vec<Hunk>, VcsError> {
    let mut hunks = Vec::with_capacity(patch.num_hunks());
    let mut seen = HashMap::new();
    for h in 0..patch.num_hunks() {
        let (hunk, _) = patch.hunk(h)?;
        let lines = hunk_model_lines(patch, h)?;
        let id = disambiguated_hunk_id(file_path, &lines, &mut seen);
        hunks.push(Hunk {
            id,
            old_start: hunk.old_start(),
            old_lines: hunk.old_lines(),
            new_start: hunk.new_start(),
            new_lines: hunk.new_lines(),
            context: hunk_context(&hunk),
            lines,
        });
    }
    Ok(hunks)
}

/// git's section heading for a hunk: the text git appends after the second
/// `@@` of the header (`@@ -a,b +c,d @@ <context>`), typically the enclosing
/// function or section. Empty when git emits none (e.g. a top-of-file hunk).
fn hunk_context(hunk: &git2::DiffHunk<'_>) -> String {
    let header = String::from_utf8_lossy(hunk.header());
    match header.split_once(" @@") {
        Some((_, rest)) => rest.trim_matches(['\n', '\r', ' ']).to_owned(),
        None => String::new(),
    }
}

/// Content lines of one hunk as model lines (headers and EOF-newline markers
/// excluded). Shared by model building and hunk lookup so the hunk ids
/// computed in both places agree.
fn hunk_model_lines(patch: &git2::Patch<'_>, h: usize) -> Result<Vec<DiffLine>, VcsError> {
    let (_, line_count) = patch.hunk(h)?;
    let mut lines = Vec::with_capacity(line_count);
    for l in 0..line_count {
        let line = patch.line_in_hunk(h, l)?;
        let kind = match line.origin() {
            '-' => LineKind::Deleted,
            '+' => LineKind::Added,
            ' ' => LineKind::Context,
            // headers, EOF-newline markers etc. are not content lines
            _ => continue,
        };
        let text = String::from_utf8_lossy(line.content())
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        lines.push(DiffLine::new(
            kind,
            line.old_lineno(),
            line.new_lineno(),
            text,
        ));
    }
    Ok(lines)
}

fn build_binary_file(diff: &git2::Diff<'_>, idx: usize) -> Option<FileDiff> {
    let delta = diff.get_delta(idx)?;
    Some(FileDiff {
        path: delta_new_path(&delta),
        old_path: None,
        status: map_status(delta.status()),
        binary: true,
        old_text: None,
        new_text: None,
        hunks: Vec::new(),
        hashes: crate::model::HashCache::default(),
    })
}

fn delta_new_path(delta: &git2::DiffDelta<'_>) -> String {
    delta
        .new_file()
        .path()
        .or_else(|| delta.old_file().path())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn map_status(status: git2::Delta) -> FileStatus {
    match status {
        git2::Delta::Added => FileStatus::Added,
        git2::Delta::Deleted => FileStatus::Deleted,
        git2::Delta::Renamed => FileStatus::Renamed,
        git2::Delta::Untracked => FileStatus::Untracked,
        _ => FileStatus::Modified,
    }
}

fn blob_text(repo: &git2::Repository, oid: git2::Oid) -> Option<String> {
    if oid.is_zero() {
        return None;
    }
    let blob = repo.find_blob(oid).ok()?;
    if blob.is_binary() {
        return None;
    }
    String::from_utf8(blob.content().to_vec()).ok()
}

/// New-side content: the recorded blob when the diff target is a tree or the
/// index (where the workdir may differ), the workdir file otherwise.
fn new_side_text(
    repo: &git2::Repository,
    delta: &git2::DiffDelta<'_>,
    rel: &str,
) -> Option<String> {
    if delta.status() == git2::Delta::Deleted {
        return None;
    }
    if let Some(text) = blob_text(repo, delta.new_file().id()) {
        return Some(text);
    }
    let root = repo.workdir()?;
    fs::read_to_string(root.join(rel)).ok()
}
