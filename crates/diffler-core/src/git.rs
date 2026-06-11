//! git2 backend for the [`Vcs`] trait: the only module that may touch git2
//! (test fixtures aside).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::model::{DiffLine, DiffModel, FileDiff, FileStatus, Hunk, LineKind, hunk_id};
use crate::vcs::{BranchInfo, HeadInfo, LogEntry, StatusModel, Vcs, VcsError};

pub struct GitVcs {
    repo: git2::Repository,
}

impl GitVcs {
    pub fn open(root: &Path) -> Result<Self, VcsError> {
        let repo = git2::Repository::open(root)?;
        if repo.workdir().is_none() {
            return Err(VcsError::NoWorkdir);
        }
        Ok(Self { repo })
    }

    /// HEAD tree, or `None` on an unborn branch (fresh repo).
    fn head_tree(&self) -> Result<Option<git2::Tree<'_>>, VcsError> {
        match self.repo.head() {
            Ok(head) => Ok(Some(head.peel_to_tree()?)),
            Err(err) if err.code() == git2::ErrorCode::UnbornBranch => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

impl Vcs for GitVcs {
    fn head(&self) -> Result<HeadInfo, VcsError> {
        match self.repo.head() {
            Ok(head) => {
                let branch = if head.is_branch() {
                    Some(head.shorthand()?.to_owned())
                } else {
                    None
                };
                let commit = head.peel_to_commit()?;
                let upstream = branch.as_deref().and_then(|name| {
                    let local = self.repo.find_branch(name, git2::BranchType::Local).ok()?;
                    let upstream = local.upstream().ok()?;
                    upstream.name().ok().flatten().map(str::to_owned)
                });
                Ok(HeadInfo {
                    branch,
                    oid7: short7(&commit.id().to_string()),
                    subject: commit.summary()?.unwrap_or_default().to_owned(),
                    upstream,
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
            .diff_index_to_workdir(None, Some(&mut workdir_diff_options()))?;
        let workdir_model = diff_to_model(&self.repo, &mut workdir)?;
        let (untracked, unstaged): (Vec<_>, Vec<_>) = workdir_model
            .files
            .into_iter()
            .partition(|f| f.status == FileStatus::Untracked);

        let head_tree = self.head_tree()?;
        let mut opts = git2::DiffOptions::new();
        opts.context_lines(3);
        let mut staged = self
            .repo
            .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?;
        let staged = diff_to_model(&self.repo, &mut staged)?;

        Ok(StatusModel {
            untracked: DiffModel { files: untracked },
            unstaged: DiffModel { files: unstaged },
            staged,
        })
    }

    fn working_tree_diff(&self) -> Result<DiffModel, VcsError> {
        let head_tree = self.head_tree()?;
        let mut diff = self.repo.diff_tree_to_workdir_with_index(
            head_tree.as_ref(),
            Some(&mut workdir_diff_options()),
        )?;
        let mut find = git2::DiffFindOptions::new();
        find.renames(true);
        diff.find_similar(Some(&mut find))?;
        diff_to_model(&self.repo, &mut diff)
    }

    fn commit_diff(&self, oid: &str) -> Result<DiffModel, VcsError> {
        let oid = git2::Oid::from_str(oid)?;
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;
        // root commit: first-parent tree is the empty tree
        let parent_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;
        let mut opts = git2::DiffOptions::new();
        opts.context_lines(3);
        let mut diff =
            self.repo
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
        diff_to_model(&self.repo, &mut diff)
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
            let Some(target) = reference.resolve().ok().and_then(|r| r.target()) else {
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

    fn branches(&self) -> Result<Vec<BranchInfo>, VcsError> {
        let mut out = Vec::new();
        for entry in self.repo.branches(Some(git2::BranchType::Local))? {
            let (branch, _) = entry?;
            let Some(name) = branch.name()?.map(str::to_owned) else {
                continue;
            };
            out.push(BranchInfo {
                name,
                is_head: branch.is_head(),
            });
        }
        Ok(out)
    }
}

fn workdir_diff_options() -> git2::DiffOptions {
    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .context_lines(3);
    opts
}

fn short7(oid: &str) -> String {
    oid.get(..7).unwrap_or(oid).to_owned()
}

fn diff_to_model(
    repo: &git2::Repository,
    diff: &mut git2::Diff<'_>,
) -> Result<DiffModel, VcsError> {
    let mut files = Vec::new();
    for idx in 0..diff.deltas().len() {
        if let Some(file) = build_file(repo, diff, idx)? {
            files.push(file);
        }
    }
    Ok(DiffModel { files })
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

    let mut hunks = Vec::new();
    for h in 0..patch.num_hunks() {
        let (hunk, line_count) = patch.hunk(h)?;
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
        let id = hunk_id(&file_path, &lines)?;
        hunks.push(Hunk {
            id,
            old_start: hunk.old_start(),
            old_lines: hunk.old_lines(),
            new_start: hunk.new_start(),
            new_lines: hunk.new_lines(),
            lines,
        });
    }

    Ok(Some(FileDiff {
        path: file_path,
        old_path,
        status,
        binary: false,
        old_text,
        new_text,
        hunks,
    }))
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
