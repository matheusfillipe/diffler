//! git2 backend for the [`Vcs`] trait.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::diffalgo::{DiffAlgorithm, DiffSettings, histogram_hunks};
use crate::model::{
    DiffLine, DiffModel, FileDiff, FileStatus, Hunk, HunkId, LineKind, disambiguated_hunk_id,
};
use crate::vcs::{
    BlameSpan, BranchInfo, FileVersion, HeadInfo, LogEntry, NetworkOp, StatusModel, Vcs, VcsError,
    Worktree, WorktreeOp,
};

pub const DEFAULT_CONTEXT_LINES: u32 = 3;

/// Commits a file history walks at most, so a file untouched for years in
/// a huge repo still answers quickly.
const HISTORY_WALK_LIMIT: usize = 20_000;

pub const DEFAULT_INDENT_HEURISTIC: bool = true;

pub struct GitVcs {
    repo: git2::Repository,
    context_lines: u32,
    /// A `Cell` so `Vcs::set_diff_algorithm` works through `&self`.
    algorithm: Cell<DiffAlgorithm>,
    indent_heuristic: Cell<bool>,
    /// False for a jj repo's own store, which has no index to read.
    indexed: bool,
    /// The commit every read of HEAD means. A jj store's own HEAD never
    /// moves, so jj sets this to `@-`.
    base: Cell<Base>,
}

#[derive(Debug, Clone, Copy)]
enum Base {
    FollowHead,
    Unborn,
    At(git2::Oid),
}

impl GitVcs {
    pub fn open(root: &Path) -> Result<Self, VcsError> {
        Self::open_with_settings(root, &DiffSettings::default())
    }

    pub fn open_with_settings(root: &Path, settings: &DiffSettings) -> Result<Self, VcsError> {
        let repo = git2::Repository::open(root)?;
        if repo.workdir().is_none() {
            return Err(VcsError::NoWorkdir);
        }
        Ok(Self {
            repo,
            context_lines: settings.context_lines,
            algorithm: Cell::new(settings.algorithm),
            indent_heuristic: Cell::new(settings.indent_heuristic),
            indexed: true,
            base: Cell::new(Base::FollowHead),
        })
    }

    /// The bare git store of a jj repo with no `.git` of its own, its
    /// worktree set to `root`.
    pub fn open_jj_store(
        git_dir: &Path,
        root: &Path,
        settings: &DiffSettings,
    ) -> Result<Self, VcsError> {
        let repo = git2::Repository::open_bare(git_dir)?;
        repo.set_workdir(root, false)?;
        // jj keeps its own state in `.jj` and, with no `.git`, writes no ignore file there
        repo.add_ignore_rule(".jj/")?;
        Ok(Self {
            repo,
            context_lines: settings.context_lines,
            algorithm: Cell::new(settings.algorithm),
            indent_heuristic: Cell::new(settings.indent_heuristic),
            indexed: false,
            base: Cell::new(Base::Unborn),
        })
    }

    /// Read every later "HEAD" as `base`, `None` being an unborn one.
    pub(crate) fn set_base(&self, base: Option<&str>) -> Result<(), VcsError> {
        self.base.set(match base {
            Some(oid) => Base::At(git2::Oid::from_str(oid)?),
            None => Base::Unborn,
        });
        Ok(())
    }

    /// `None` on an unborn branch or base.
    fn head_commit(&self) -> Result<Option<git2::Commit<'_>>, VcsError> {
        match self.base.get() {
            Base::At(oid) => Ok(Some(self.repo.find_commit(oid)?)),
            Base::Unborn => Ok(None),
            Base::FollowHead => match self.repo.head() {
                Ok(head) => Ok(Some(head.peel_to_commit()?)),
                Err(err) if err.code() == git2::ErrorCode::UnbornBranch => Ok(None),
                Err(err) => Err(err.into()),
            },
        }
    }

    fn head_commit_or_unborn(&self) -> Result<git2::Commit<'_>, VcsError> {
        self.head_commit()?.ok_or_else(|| {
            git2::Error::new(
                git2::ErrorCode::UnbornBranch,
                git2::ErrorClass::Reference,
                "HEAD has no commit yet",
            )
            .into()
        })
    }

    /// `rev`, with a leading `HEAD` (`HEAD`, `HEAD~2`, `HEAD^`) read as the
    /// base when one is set.
    fn revparse(&self, rev: &str) -> Result<git2::Object<'_>, VcsError> {
        let head_relative = rev
            .strip_prefix("HEAD")
            .filter(|rest| rest.is_empty() || rest.starts_with(['~', '^']));
        match (self.base.get(), head_relative) {
            (Base::At(_) | Base::Unborn, Some(rest)) => {
                let oid = self.head_commit_or_unborn()?.id();
                Ok(self.repo.revparse_single(&format!("{oid}{rest}"))?)
            }
            _ => Ok(self.repo.revparse_single(rev)?),
        }
    }

    fn tree_files(&self) -> Result<Vec<PathBuf>, VcsError> {
        let Some(tree) = self.head_tree()? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob)
                && let Ok(name) = entry.name()
            {
                out.push(PathBuf::from(format!("{dir}{name}")));
            }
            git2::TreeWalkResult::Ok
        })?;
        out.sort_unstable();
        Ok(out)
    }

    /// `None` on an unborn branch.
    fn head_tree(&self) -> Result<Option<git2::Tree<'_>>, VcsError> {
        Ok(self
            .head_commit()?
            .map(|commit| commit.tree())
            .transpose()?)
    }

    fn workdir_path(&self) -> Result<&Path, VcsError> {
        self.repo.workdir().ok_or(VcsError::NoWorkdir)
    }

    /// `base` tree vs index + worktree, untracked included, renames found.
    /// `None` is the empty tree.
    fn workdir_diff(&self, base: Option<&git2::Tree<'_>>) -> Result<DiffModel, VcsError> {
        let mut options = self.workdir_diff_options();
        let mut diff = if self.indexed {
            self.repo
                .diff_tree_to_workdir_with_index(base, Some(&mut options))?
        } else {
            self.repo.diff_tree_to_workdir(base, Some(&mut options))?
        };
        let sparse = self.sparse_paths();
        // we skip rename detection in a sparse checkout, since it would read
        // every file outside the cone, which a partial clone never downloaded
        if sparse.is_empty() {
            let mut find = git2::DiffFindOptions::new();
            find.renames(true);
            diff.find_similar(Some(&mut find))?;
        }
        let mut model = self.diff_to_model_skipping(&mut diff, &sparse)?;
        if !self.indexed {
            // jj tracks every file of the working copy that no ignore rule names
            for file in &mut model.files {
                if file.status == FileStatus::Untracked {
                    file.status = FileStatus::Added;
                }
            }
        }
        Ok(model)
    }

    /// The index entries a sparse checkout leaves out of the worktree. libgit2
    /// ignores the skip-worktree bit, so we have to drop the deletions it
    /// reports for them; git shows none.
    fn sparse_paths(&self) -> HashSet<Vec<u8>> {
        let Ok(index) = self.repo.index() else {
            return HashSet::new();
        };
        index
            .iter()
            .filter(|entry| {
                git2::IndexEntryExtendedFlag::from_bits_truncate(entry.flags_extended)
                    .is_skip_worktree()
            })
            .map(|entry| entry.path)
            .collect()
    }

    fn plain_diff_options(&self) -> git2::DiffOptions {
        let mut opts = git2::DiffOptions::new();
        opts.context_lines(self.context_lines);
        apply_git_algorithm(&mut opts, self.algorithm.get(), self.indent_heuristic.get());
        opts
    }

    fn workdir_diff_options(&self) -> git2::DiffOptions {
        let mut opts = self.plain_diff_options();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        opts
    }

    /// Leaves `.emphasis` empty; the TUI enriches a file when it draws it.
    fn diff_to_model(&self, diff: &mut git2::Diff<'_>) -> Result<DiffModel, VcsError> {
        self.diff_to_model_skipping(diff, &HashSet::new())
    }

    /// [`Self::diff_to_model`] for a worktree diff, leaving out the deletions
    /// of the `sparse` paths a sparse checkout never wrote to disk.
    fn diff_to_model_skipping(
        &self,
        diff: &mut git2::Diff<'_>,
        sparse: &HashSet<Vec<u8>>,
    ) -> Result<DiffModel, VcsError> {
        let mut files = Vec::new();
        for idx in 0..diff.deltas().len() {
            let outside_cone = diff.get_delta(idx).is_some_and(|delta| {
                delta.status() == git2::Delta::Deleted
                    && sparse.contains(delta.old_file().path_bytes().unwrap_or_default())
            });
            if outside_cone {
                continue;
            }
            if let Some(file) = self.build_file(diff, idx)? {
                files.push(file);
            }
        }
        Ok(DiffModel { files })
    }

    /// A modified text file's hunks under histogram/structural, which libgit2
    /// lacks; `None` keeps git2's own. Model building and hunk staging both
    /// call this so their hunk ids agree.
    fn imara_hunks(
        &self,
        delta: &git2::DiffDelta<'_>,
        old: Option<&str>,
        new: Option<&str>,
        path: &str,
    ) -> Option<Vec<Hunk>> {
        if !self.algorithm.get().is_imara() {
            return None;
        }
        let (old, new) = (old?, new?);
        // we read the worktree side raw, so when a clean filter (autocrlf,
        // ident) makes git compare other bytes we keep git's own hunks
        let hashed = git2::Oid::hash_object(git2::ObjectType::Blob, new.as_bytes()).ok()?;
        if hashed != delta.new_file().id() {
            return None;
        }
        Some(histogram_hunks(
            old,
            new,
            path,
            self.context_lines,
            self.indent_heuristic.get(),
        ))
    }

    fn walk_entries(
        &self,
        walk: git2::Revwalk<'_>,
        limit: usize,
    ) -> Result<Vec<LogEntry>, VcsError> {
        let mut entries = Vec::new();
        for oid in walk.take(limit) {
            entries.push(log_entry(&self.repo.find_commit(oid?)?)?);
        }
        Ok(entries)
    }

    /// Where `commit` took `path` from its first parent: the same path, or
    /// the one a rename moved it from. `None` when the parent has neither.
    fn path_in_parent(
        &self,
        commit: &git2::Commit<'_>,
        parent: &git2::Commit<'_>,
        path: &str,
    ) -> Result<Option<String>, VcsError> {
        if blob_at(parent, path)?.is_some() {
            return Ok(Some(path.to_owned()));
        }
        let mut diff = self.repo.diff_tree_to_tree(
            Some(&parent.tree()?),
            Some(&commit.tree()?),
            Some(&mut git2::DiffOptions::new()),
        )?;
        diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;
        Ok(diff.deltas().find_map(|delta| {
            let renamed = delta.status() == git2::Delta::Renamed
                && delta.new_file().path() == Some(Path::new(path));
            renamed
                .then(|| {
                    delta
                        .old_file()
                        .path()
                        .map(|old| old.to_string_lossy().into_owned())
                })
                .flatten()
        }))
    }

    fn blame_spans(&self, blame: &git2::Blame<'_>) -> Vec<BlameSpan> {
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
        out
    }

    /// [`Vcs::log`], with `all_branches` also walking `tips`, the commits jj
    /// bookmarks that the git store holds no ref for.
    pub(crate) fn log_from(
        &self,
        limit: usize,
        all_branches: bool,
        tips: &[&str],
    ) -> Result<Vec<LogEntry>, VcsError> {
        let Some(head) = self.head_commit()? else {
            return Ok(Vec::new());
        };
        let mut refs_by_oid: HashMap<git2::Oid, Vec<String>> = HashMap::new();
        for reference in self.repo.references()?.flatten() {
            let Ok(name) = reference.shorthand().map(str::to_owned) else {
                continue;
            };
            let Some(target) = reference.peel_to_commit().ok().map(|c| c.id()) else {
                continue;
            };
            refs_by_oid.entry(target).or_default().push(name);
        }

        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        walk.push(head.id())?;
        if all_branches {
            for glob in ["refs/heads", "refs/remotes", "refs/tags"] {
                walk.push_glob(glob)?;
            }
            for tip in tips {
                walk.push(git2::Oid::from_str(tip)?)?;
            }
        }
        let mut entries = self.walk_entries(walk, limit)?;
        for entry in &mut entries {
            if let Some(refs) = git2::Oid::from_str(&entry.oid)
                .ok()
                .and_then(|oid| refs_by_oid.remove(&oid))
            {
                entry.refs = refs;
            }
        }
        Ok(entries)
    }

    /// `(ahead, behind)` of commit `local` against commit `upstream`.
    pub(crate) fn ahead_behind(
        &self,
        local: &str,
        upstream: &str,
    ) -> Result<(usize, usize), VcsError> {
        Ok(self
            .repo
            .graph_ahead_behind(git2::Oid::from_str(local)?, git2::Oid::from_str(upstream)?)?)
    }

    /// Whether `git stash` would save anything; untracked files don't count.
    fn has_tracked_changes(&self) -> Result<bool, VcsError> {
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(false).include_ignored(false);
        let statuses = self.repo.statuses(Some(&mut opts))?;
        let sparse = self.sparse_paths();
        Ok(statuses.iter().any(|entry| {
            !(entry.status() == git2::Status::WT_DELETED && sparse.contains(entry.path_bytes()))
        }))
    }
}

impl Vcs for GitVcs {
    fn has_index(&self) -> bool {
        true
    }

    fn native_git_checkout(&self) -> bool {
        true
    }

    fn git_dir(&self) -> Result<PathBuf, VcsError> {
        Ok(self.repo.path().to_path_buf())
    }

    fn head(&self) -> Result<HeadInfo, VcsError> {
        if !matches!(self.base.get(), Base::FollowHead) {
            let commit = self.head_commit()?;
            return Ok(HeadInfo {
                branch: None,
                oid7: commit
                    .as_ref()
                    .map(|c| short7(&c.id().to_string()))
                    .unwrap_or_default(),
                subject: commit
                    .as_ref()
                    .and_then(|c| c.summary().ok().flatten().map(str::to_owned))
                    .unwrap_or_default(),
                upstream: None,
                ahead: 0,
                behind: 0,
            });
        }
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
        // we diff against the index so a staged new file lands in staged only
        let mut workdir = self
            .repo
            .diff_index_to_workdir(None, Some(&mut self.workdir_diff_options()))?;
        let workdir_model = self.diff_to_model_skipping(&mut workdir, &self.sparse_paths())?;
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
        let object = self.revparse(revision)?;
        let commit = object.peel_to_commit()?;
        Ok(commit.id().to_string())
    }

    fn range_diff(&self, oldest_oid: &str, newest_oid: &str) -> Result<DiffModel, VcsError> {
        let oldest = self.repo.find_commit(git2::Oid::from_str(oldest_oid)?)?;
        let newest = self.repo.find_commit(git2::Oid::from_str(newest_oid)?)?;
        let newest_tree = newest.tree()?;
        let base_tree = oldest.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut diff = self.repo.diff_tree_to_tree(
            base_tree.as_ref(),
            Some(&newest_tree),
            Some(&mut self.plain_diff_options()),
        )?;
        self.diff_to_model(&mut diff)
    }

    fn log(&self, limit: usize, all_branches: bool) -> Result<Vec<LogEntry>, VcsError> {
        self.log_from(limit, all_branches, &[])
    }

    fn default_branch(&self, remote: &str) -> Result<Option<String>, VcsError> {
        let head_ref = format!("refs/remotes/{remote}/HEAD");
        let prefix = format!("refs/remotes/{remote}/");
        if let Ok(reference) = self.repo.find_reference(&head_ref)
            && let Ok(Some(target)) = reference.symbolic_target()
            // we keep the whole remainder so `release/2.x` survives
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
        let (base, head) = (self.revparse(base)?, self.revparse(head)?);
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        walk.push(head.id())?;
        walk.hide(base.id())?;
        self.walk_entries(walk, usize::MAX)
    }

    fn unpushed(&self, limit: usize) -> Result<Option<Vec<LogEntry>>, VcsError> {
        let Some(head) = self.head_commit().ok().flatten() else {
            return Ok(None);
        };
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        walk.push(head.id())?;
        let mut remotes = 0;
        for reference in self.repo.references()? {
            let reference = reference?;
            // a symbolic ref (origin/HEAD) has no target, and its destination
            // is hidden anyway
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
        if !matches!(self.base.get(), Base::FollowHead) {
            options.newest_commit(self.head_commit_or_unborn()?.id());
        }
        let blame = self.repo.blame_file(rel, Some(&mut options))?;
        // blame_file only knows committed content, so we remap the spans onto
        // the worktree with blame_buffer
        let workdir = self.workdir()?.join(rel);
        let blame = match fs::read(&workdir) {
            Ok(bytes) => blame.blame_buffer(&bytes)?,
            Err(_) => blame,
        };
        Ok(self.blame_spans(&blame))
    }

    fn blame_at(&self, rev: &str, rel: &Path) -> Result<Vec<BlameSpan>, VcsError> {
        let commit = self.revparse(rev)?.peel_to_commit()?;
        let mut options = git2::BlameOptions::new();
        options
            .track_copies_same_file(true)
            .newest_commit(commit.id());
        let blame = self.repo.blame_file(rel, Some(&mut options))?;
        Ok(self.blame_spans(&blame))
    }

    fn file_history(&self, rel: &str, limit: usize) -> Result<Vec<FileVersion>, VcsError> {
        let mut versions = Vec::new();
        let Some(mut commit) = self.head_commit()? else {
            return Ok(versions);
        };
        let mut path = rel.to_owned();
        for _ in 0..HISTORY_WALK_LIMIT {
            let Some(blob) = blob_at(&commit, &path)? else {
                break;
            };
            let parent = commit.parent(0).ok();
            let unchanged = match &parent {
                Some(parent) => blob_at(parent, &path)? == Some(blob),
                None => false,
            };
            if !unchanged {
                versions.push(FileVersion {
                    entry: log_entry(&commit)?,
                    path: path.clone(),
                });
                if versions.len() >= limit {
                    break;
                }
            }
            let Some(parent) = parent else {
                break;
            };
            if !unchanged {
                match self.path_in_parent(&commit, &parent, &path)? {
                    Some(older) => path = older,
                    None => break,
                }
            }
            commit = parent;
        }
        Ok(versions)
    }

    fn read_at(&self, rev: &str, path: &str) -> Result<Option<String>, VcsError> {
        let commit = self.revparse(rev)?.peel_to_commit()?;
        Ok(blob_at(&commit, path)?.and_then(|oid| blob_text(&self.repo, oid)))
    }

    fn read_blob(&self, oid: &str) -> Result<Option<Vec<u8>>, VcsError> {
        let oid = git2::Oid::from_str(oid)?;
        match self.repo.find_blob(oid) {
            Ok(blob) => Ok(Some(blob.content().to_vec())),
            Err(err) if err.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn tracked_files(&self) -> Result<Vec<PathBuf>, VcsError> {
        if !self.indexed {
            return self.tree_files();
        }
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
        // a bare `linguist-generated` is git's boolean, `=true` is a string
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
            // refs/remotes/<remote>/HEAD is a symbolic alias
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
        let sparse = self.sparse_paths();
        let mut index = self.repo.index()?;
        if sparse.is_empty() {
            // update_all catches deletions, add_all catches untracked files
            index.update_all(["*"], None)?;
            index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
        } else {
            // both read the blob of every path outside the sparse cone, which a
            // partial clone never downloaded, so we stage the worktree diff's
            // paths ourselves; listing the deltas reads no blob
            let workdir = self.workdir_path()?.to_path_buf();
            let diff = self
                .repo
                .diff_index_to_workdir(Some(&index), Some(&mut self.workdir_diff_options()))?;
            let changed: Vec<PathBuf> = diff
                .deltas()
                .filter_map(|delta| {
                    let file = if delta.status() == git2::Delta::Deleted {
                        delta.old_file()
                    } else {
                        delta.new_file()
                    };
                    let path = file.path_bytes()?;
                    (!sparse.contains(path))
                        .then(|| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
                })
                .collect();
            for path in changed {
                if workdir.join(&path).exists() {
                    index.add_path(&path)?;
                } else {
                    index.remove_path(&path)?;
                }
            }
        }
        index.write()?;
        Ok(())
    }

    fn unstage_everything(&self) -> Result<(), VcsError> {
        match self.repo.head() {
            Ok(head) => {
                let target = head.peel(git2::ObjectType::Commit)?;
                self.repo.reset_default(Some(&target), ["*"])?;
            }
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
        // we update the index stat cache too: under autocrlf the checkout
        // grows the file, and a stale cached size makes git report a phantom
        // modification. Nothing is staged here, so only metadata changes.
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
        let commit = self.head_commit_or_unborn()?;
        Ok(commit.message().unwrap_or_default().to_owned())
    }

    fn commit_message(&self, oid: &str) -> Result<String, VcsError> {
        let commit = self.repo.find_commit(git2::Oid::from_str(oid)?)?;
        Ok(commit.message().unwrap_or_default().to_owned())
    }

    fn amend(&self, message: Option<&str>, use_index: bool) -> Result<String, VcsError> {
        if let Some(message) = message
            && message.trim().is_empty()
        {
            return Err(VcsError::Rejected("empty commit message".into()));
        }
        let head = self.repo.head()?.peel_to_commit()?;
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
        // a non-force checkout refuses to clobber local modifications
        self.repo.checkout_tree(&target, None)?;
        self.repo.set_head(&format!("refs/heads/{name}"))?;
        Ok(())
    }

    fn stash_push(&self, message: Option<&str>) -> Result<(), VcsError> {
        if !self.has_tracked_changes()? {
            return Err(VcsError::Rejected("nothing to stash".into()));
        }
        // git2's stash needs `&mut Repository` and the trait is `&self`, so we
        // open a fresh handle
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
            // stash entry
            Err(err) if err.code() == git2::ErrorCode::Conflict => Err(VcsError::Rejected(
                "stash applied with conflicts; resolve them (the stash was kept)".into(),
            )),
            Err(err) => Err(err.into()),
        }
    }

    fn network_argv(&self, op: NetworkOp) -> Result<Vec<String>, VcsError> {
        let args: Vec<String> = match op {
            NetworkOp::Fetch => vec!["fetch".into()],
            NetworkOp::FetchAll => vec!["fetch".into(), "--all".into()],
            NetworkOp::Push => vec!["push".into()],
            NetworkOp::PushSetUpstream { remote } => {
                vec!["push".into(), "-u".into(), remote, "HEAD".into()]
            }
            NetworkOp::Pull => vec!["pull".into()],
            NetworkOp::PullFrom { remote, branch } => vec!["pull".into(), remote, branch],
            NetworkOp::PullRebase => vec!["pull".into(), "--rebase".into()],
            NetworkOp::PullMerge => vec!["pull".into(), "--no-rebase".into()],
        };
        Ok(std::iter::once("git".to_owned()).chain(args).collect())
    }

    fn worktrees(&self) -> Result<Vec<Worktree>, VcsError> {
        let here = self
            .repo
            .workdir()
            .and_then(|dir| fs::canonicalize(dir).ok());
        let opened;
        let main = if self.repo.is_worktree() {
            opened = git2::Repository::open(self.repo.commondir())?;
            &opened
        } else {
            &self.repo
        };
        let entry = |name: String, path: &Path, repo: Option<&git2::Repository>, main: bool| {
            let (label, subject, time_unix) =
                repo.map_or_else(|| (name.clone(), String::new(), 0), worktree_head);
            let branch = repo
                .and_then(|repo| repo.head().ok())
                .filter(git2::Reference::is_branch)
                .and_then(|head| head.shorthand().ok().map(str::to_owned));
            Worktree {
                name,
                current: here.is_some() && fs::canonicalize(path).ok() == here,
                path: path.to_path_buf(),
                label,
                branch,
                subject,
                time_unix,
                main,
                missing: repo.is_none(),
            }
        };
        let mut found = Vec::new();
        // a bare clone has no main worktree, only linked ones
        if let Some(dir) = main.workdir() {
            found.push(entry(String::new(), dir, Some(main), true));
        }
        let mut names: Vec<String> = main
            .worktrees()?
            .iter()
            .flatten()
            .flatten()
            .map(str::to_owned)
            .collect();
        names.sort();
        for name in names {
            let linked = main.find_worktree(&name)?;
            let repo = linked
                .validate()
                .ok()
                .and_then(|()| git2::Repository::open_from_worktree(&linked).ok());
            found.push(entry(name, linked.path(), repo.as_ref(), false));
        }
        Ok(found)
    }

    fn worktree_argv(&self, op: &WorktreeOp) -> Result<Vec<String>, VcsError> {
        let mut argv: Vec<String> = vec!["git".into(), "worktree".into()];
        match op {
            WorktreeOp::Add {
                path,
                branch,
                new_branch: true,
            } => argv.extend([
                "add".into(),
                "-b".into(),
                branch.clone(),
                path.display().to_string(),
            ]),
            WorktreeOp::Add { path, branch, .. } => {
                argv.extend(["add".into(), path.display().to_string(), branch.clone()]);
            }
            WorktreeOp::Remove { worktree, .. } if worktree.main => {
                return Err(VcsError::Rejected(
                    "the main worktree stays with the repository; remove a linked one".into(),
                ));
            }
            WorktreeOp::Remove { worktree, force } => {
                argv.push("remove".into());
                if *force {
                    argv.push("--force".into());
                }
                argv.push(worktree.path.display().to_string());
            }
        }
        Ok(argv)
    }

    fn workdir(&self) -> Result<PathBuf, VcsError> {
        Ok(self.workdir_path()?.to_path_buf())
    }

    fn remote_url(&self, name: &str) -> Result<Option<String>, VcsError> {
        match self.repo.find_remote(name) {
            // url() errs only on a non-UTF-8 URL, which we treat as none
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
}

/// Histogram and structural have no git2 flag; [`GitVcs::imara_hunks`]
/// computes those.
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
    fn synthesize_patch(
        &self,
        diff: &git2::Diff<'_>,
        rel: &Path,
        target: &HunkId,
        reverse: bool,
    ) -> Result<Vec<u8>, VcsError> {
        let rel = rel.to_string_lossy();
        for idx in 0..diff.deltas().len() {
            let Some(patch) = git2::Patch::from_diff(diff, idx)? else {
                continue;
            };
            // we take `delta` from the patch, since loading the patch fills in
            // the worktree side's id that `imara_hunks` checks
            let delta = patch.delta();
            if delta.flags().is_binary() || delta_new_path(&delta) != rel {
                continue;
            }
            if self.algorithm.get().is_imara() {
                let old_text = blob_text(&self.repo, delta.old_file().id());
                let new_text = new_side_text(&self.repo, &delta, &rel);
                let (old, new) = (old_text.as_deref(), new_text.as_deref());
                if let Some(hunks) = self.imara_hunks(&delta, old, new, &rel)
                    && let (Some(old), Some(new)) = (old, new)
                {
                    if let Some(hunk) = hunks.iter().find(|h| h.id == *target) {
                        return Ok(render_hunk_patch_from_model(hunk, &rel, reverse, old, new));
                    }
                    continue;
                }
            }
            let mut seen = HashMap::new();
            for h in 0..patch.num_hunks() {
                let lines = hunk_model_lines(&patch, h)?;
                if disambiguated_hunk_id(&rel, &lines, &mut seen) == *target {
                    return render_hunk_patch(&patch, h, &rel, delta.status(), reverse);
                }
            }
        }
        Err(VcsError::Rejected("hunk not found (diff changed?)".into()))
    }

    fn build_file(
        &self,
        diff: &mut git2::Diff<'_>,
        idx: usize,
    ) -> Result<Option<FileDiff>, VcsError> {
        let repo = &self.repo;
        let Some(patch) = git2::Patch::from_diff(diff, idx)? else {
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

        let imara = self.imara_hunks(&delta, old_text.as_deref(), new_text.as_deref(), &file_path);
        let hunks = match imara {
            Some(hunks) => hunks,
            None => patch_hunks(&patch, &file_path)?,
        };

        Ok(Some(FileDiff {
            path: file_path,
            old_path,
            status,
            binary: false,
            old_text,
            new_text,
            hunks,
            hashes: crate::model::HashCache::default(),
            blobs: crate::model::BlobIds::default(),
        }))
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
    // whole-file adds and deletes keep that identity (sides swapped under
    // reverse) so applying creates or drops the index entry; otherwise git
    // leaves an empty blob behind
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
    let mut lines: Vec<(LineKind, Vec<u8>)> = Vec::with_capacity(line_count);
    for l in 0..line_count {
        let line = patch.line_in_hunk(h, l)?;
        let kind = match line.origin() {
            ' ' => LineKind::Context,
            '+' => LineKind::Added,
            '-' => LineKind::Deleted,
            // an EOF-newline marker carries the "\ No newline" text and the
            // newline ending the previous line, so we append it to that line
            '=' | '>' | '<' => {
                if let Some((_, text)) = lines.last_mut() {
                    text.extend_from_slice(line.content());
                }
                continue;
            }
            _ => continue,
        };
        lines.push((kind, line.content().to_vec()));
    }
    write_hunk(
        &mut out,
        (hunk.old_start(), hunk.old_lines()),
        (hunk.new_start(), hunk.new_lines()),
        &lines,
        reverse,
    );
    Ok(out)
}

/// One hunk's `@@` header and lines. `old`/`new` are `(start, count)`; each
/// line's bytes are already terminated.
fn write_hunk(
    out: &mut Vec<u8>,
    old: (u32, u32),
    new: (u32, u32),
    lines: &[(LineKind, Vec<u8>)],
    reverse: bool,
) {
    let ((minus_start, minus_lines), (_, plus_lines)) =
        if reverse { (new, old) } else { (old, new) };
    // libgit2 applies a hunk at its `+` start, and we send this hunk without
    // the ones before it, so we point that start at the minus side's
    // position; git's header gives an empty side the line before the change
    let plus_start = if minus_lines == 0 {
        minus_start + 1
    } else {
        minus_start
    };
    out.extend_from_slice(
        format!("@@ -{minus_start},{minus_lines} +{plus_start},{plus_lines} @@\n").as_bytes(),
    );
    // minus lines go first in a change run even under `reverse`: libgit2
    // rejects a `+` line marked "No newline at end of file" before a `-` one
    let (minus_kind, plus_kind) = if reverse {
        (LineKind::Added, LineKind::Deleted)
    } else {
        (LineKind::Deleted, LineKind::Added)
    };
    let is_context = |kind: LineKind| kind == LineKind::Context;
    for run in lines.chunk_by(|a, b| is_context(a.0) == is_context(b.0)) {
        for (origin, kind) in [
            (b' ', LineKind::Context),
            (b'-', minus_kind),
            (b'+', plus_kind),
        ] {
            for (_, text) in run.iter().filter(|(k, _)| *k == kind) {
                out.push(origin);
                out.extend_from_slice(text);
            }
        }
    }
}

/// [`render_hunk_patch`] for an imara-diff hunk. We copy each line's raw
/// bytes from `old_text`/`new_text`, since the model's text has CR and LF
/// stripped.
fn render_hunk_patch_from_model(
    hunk: &Hunk,
    rel: &str,
    reverse: bool,
    old_text: &str,
    new_text: &str,
) -> Vec<u8> {
    let old_raw: Vec<&str> = old_text.split_inclusive('\n').collect();
    let new_raw: Vec<&str> = new_text.split_inclusive('\n').collect();
    let raw = |line: &DiffLine| -> Vec<u8> {
        let (side, number) = match line.kind {
            LineKind::Added => (&new_raw, line.new_no),
            LineKind::Deleted | LineKind::Context => (&old_raw, line.old_no),
        };
        let text = number
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| side.get(i as usize))
            .copied()
            .unwrap_or_default();
        let mut bytes = text.as_bytes().to_vec();
        if !text.ends_with('\n') {
            bytes.extend_from_slice(b"\n\\ No newline at end of file\n");
        }
        bytes
    };
    let lines: Vec<(LineKind, Vec<u8>)> = hunk.lines.iter().map(|l| (l.kind, raw(l))).collect();

    let mut out = format!("diff --git a/{rel} b/{rel}\n--- a/{rel}\n+++ b/{rel}\n").into_bytes();
    write_hunk(
        &mut out,
        (hunk.old_start, hunk.old_lines),
        (hunk.new_start, hunk.new_lines),
        &lines,
        reverse,
    );
    out
}

/// `(label, subject, time)` of a worktree's HEAD: the branch, or the commit a
/// detached HEAD sits on.
fn worktree_head(repo: &git2::Repository) -> (String, String, i64) {
    let Ok(head) = repo.head() else {
        return ("(no commits)".to_owned(), String::new(), 0);
    };
    let Ok(commit) = head.peel_to_commit() else {
        return ("(no commits)".to_owned(), String::new(), 0);
    };
    let label = if head.is_branch() {
        head.shorthand().unwrap_or_default().to_owned()
    } else {
        format!("detached {}", short7(&commit.id().to_string()))
    };
    (
        label,
        commit
            .summary()
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_owned(),
        commit.time().seconds(),
    )
}

pub fn short7(oid: &str) -> String {
    oid.get(..7).unwrap_or(oid).to_owned()
}

/// Re-diff a file's own old/new text at `context` lines (`u32::MAX` for the
/// whole file). `None` for binary files or a missing side.
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
    // libgit2's context math overflows on u32::MAX and yields zero context on
    // some platforms, so we cap it at the line count
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

/// Shared by the initial diff and the context re-diff so their ids agree.
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

/// The text after the header's second `@@`, empty when git emits none.
fn hunk_context(hunk: &git2::DiffHunk<'_>) -> String {
    let header = String::from_utf8_lossy(hunk.header());
    match header.split_once(" @@") {
        Some((_, rest)) => rest.trim_matches(['\n', '\r', ' ']).to_owned(),
        None => String::new(),
    }
}

/// Shared by model building and hunk lookup so their hunk ids agree.
fn hunk_model_lines(patch: &git2::Patch<'_>, h: usize) -> Result<Vec<DiffLine>, VcsError> {
    let (_, line_count) = patch.hunk(h)?;
    let mut lines = Vec::with_capacity(line_count);
    for l in 0..line_count {
        let line = patch.line_in_hunk(h, l)?;
        let kind = match line.origin() {
            '-' => LineKind::Deleted,
            '+' => LineKind::Added,
            ' ' => LineKind::Context,
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
    let blob = |file: git2::DiffFile<'_>| (!file.id().is_zero()).then(|| file.id().to_string());
    let blobs = crate::model::BlobIds {
        old: blob(delta.old_file()),
        new: blob(delta.new_file()),
    };
    Some(FileDiff {
        path: delta_new_path(&delta),
        old_path: None,
        status: map_status(delta.status()),
        binary: true,
        old_text: None,
        new_text: None,
        hunks: Vec::new(),
        hashes: crate::model::HashCache::default(),
        blobs,
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

fn log_entry(commit: &git2::Commit<'_>) -> Result<LogEntry, VcsError> {
    let full = commit.id().to_string();
    Ok(LogEntry {
        oid7: short7(&full),
        oid: full,
        refs: Vec::new(),
        parents: commit.parent_ids().map(|id| id.to_string()).collect(),
        subject: commit.summary()?.unwrap_or_default().to_owned(),
        author: commit.author().name().unwrap_or_default().to_owned(),
        time_unix: commit.time().seconds(),
        change_id: None,
    })
}

/// The blob `path` names in `commit`'s tree, `None` when the tree lacks it.
fn blob_at(commit: &git2::Commit<'_>, path: &str) -> Result<Option<git2::Oid>, VcsError> {
    match commit.tree()?.get_path(Path::new(path)) {
        Ok(entry) => Ok(Some(entry.id())),
        Err(err) if err.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(err) => Err(err.into()),
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

/// The recorded blob when the diff target is a tree or the index, else the
/// worktree file.
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
