//! Facade over the VCS backend, session and store, used by the TUI and MCP
//! layers.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::diffalgo::{DiffAlgorithm, DiffSettings};
use crate::model::DiffModel;
use crate::repo;
use crate::session::Session;
use crate::source::ReviewSource;
use crate::store::{self, StoreError};
use crate::vcs::{StatusModel, Vcs, VcsError};

#[derive(Debug, Error)]
pub enum ReviewError {
    #[error(transparent)]
    Vcs(#[from] VcsError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// One file as the file view reads it: worktree text plus per-line blame.
#[derive(Debug)]
pub struct FileSnapshot {
    pub path: String,
    pub content: String,
    /// Empty when git has nothing to attribute, e.g. an untracked file.
    pub blame: Vec<crate::vcs::BlameSpan>,
}

#[derive(Debug, Default)]
pub struct WalkthroughFiles {
    pub contents: HashMap<String, String>,
    /// A `rev` was named but no longer resolves (a squash, a rebase, a gc), so
    /// every file fell back to the worktree. `false` when nothing was pinned.
    pub pin_broken: bool,
}

pub const MAX_PREVIEW_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinarySide {
    Bytes(Vec<u8>),
    /// The side exists but is over [`MAX_PREVIEW_BYTES`]; its size in bytes.
    TooLarge(u64),
}

impl BinarySide {
    fn of(bytes: Vec<u8>) -> Self {
        let size = bytes.len() as u64;
        if size > MAX_PREVIEW_BYTES {
            Self::TooLarge(size)
        } else {
            Self::Bytes(bytes)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BinarySides {
    pub old: Option<BinarySide>,
    pub new: Option<BinarySide>,
}

/// Which copy of a walkthrough's file [`Review::compute_walkthrough_files`]
/// reads first, falling back to the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFirst {
    Pin,
    Worktree,
}

#[derive(Debug)]
pub struct Refreshed {
    pub status: StatusModel,
    pub model: DiffModel,
    /// The [`ReviewSource::Against`] rev asked about and its diff. We carry
    /// the rev so a review swapped while the worker ran drops the answer.
    pub against: Option<(String, Result<DiffModel, VcsError>)>,
    pub pinned: Option<Result<DiffModel, VcsError>>,
}

/// The diff for a commit, range or PR source. `pr_head` is the PR's
/// `(merge_base, head)`; `None` rejects the PR so it never reads as
/// unchanged. Other sources read back empty.
pub fn pinned_diff(
    vcs: &dyn Vcs,
    source: &ReviewSource,
    pr_head: Option<(&str, &str)>,
) -> Result<DiffModel, VcsError> {
    match source {
        ReviewSource::Commit { oid } => vcs.commit_diff(oid),
        ReviewSource::Range { oldest, newest } => vcs.range_diff(oldest, newest),
        ReviewSource::Pr { number } => {
            let (base, head) = pr_head
                .ok_or_else(|| VcsError::Rejected(format!("PR #{number} is not resolved")))?;
            vcs.tree_diff(base, head)
        }
        ReviewSource::WorkingTree
        | ReviewSource::Walkthrough { .. }
        | ReviewSource::Against { .. } => Ok(DiffModel::default()),
    }
}

pub struct Review {
    pub repo_root: PathBuf,
    pub vcs: Box<dyn Vcs>,
    pub status: StatusModel,
    /// Computed on first [`Review::model`] access, since the status screen
    /// opens first and needs no working diff.
    model: OnceCell<DiffModel>,
    /// The working-tree review session.
    pub session: Session,
    /// Sessions of every other source, keyed by [`ReviewSource::key`].
    sources: HashMap<String, (ReviewSource, Session)>,
    empty: Session,
}

impl Review {
    pub fn open(repo_root: &Path) -> Result<Self, ReviewError> {
        Self::open_with_settings(repo_root, &DiffSettings::default())
    }

    pub fn open_with_settings(
        repo_root: &Path,
        settings: &DiffSettings,
    ) -> Result<Self, ReviewError> {
        let vcs = repo::open_with_settings(repo_root, settings)?;
        let status = vcs.status()?;
        let session = store::load(repo_root)?;
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            vcs,
            status,
            model: OnceCell::new(),
            session,
            sources: HashMap::new(),
            empty: Session::default(),
        })
    }

    pub fn set_diff_algorithm(&self, algorithm: DiffAlgorithm, indent_heuristic: bool) {
        self.vcs.set_diff_algorithm(algorithm, indent_heuristic);
    }

    /// The working-tree diff, cached on first access. A backend error caches
    /// an empty diff until the next [`Review::refresh`].
    pub fn model(&self) -> &DiffModel {
        self.model
            .get_or_init(|| self.vcs.working_tree_diff().unwrap_or_default())
    }

    pub fn model_mut(&mut self) -> &mut DiffModel {
        self.model();
        #[allow(clippy::expect_used)]
        self.model.get_mut().expect("model just initialized")
    }

    /// Recompute status and diff, and drop viewed marks for files that changed
    /// or left the diff.
    pub fn refresh(&mut self) -> Result<(), ReviewError> {
        self.status = self.vcs.status()?;
        let model = self.vcs.working_tree_diff()?;
        self.install_refresh(self.status.clone(), model);
        Ok(())
    }

    /// A refresh on its own repo handle, for a worker thread; apply it with
    /// [`Review::install_refresh`]. `against` and `pinned` re-diff the open
    /// three-dot or pinned source in the same pass.
    pub fn compute_refresh(
        repo_root: &Path,
        settings: &DiffSettings,
        against: Option<&str>,
        pinned: Option<(&ReviewSource, Option<(&str, &str)>)>,
    ) -> Result<Refreshed, ReviewError> {
        let vcs = repo::open_with_settings(repo_root, settings)?;
        let status = vcs.status()?;
        let model = vcs.working_tree_diff()?;
        let against =
            against.map(|rev| (rev.to_owned(), crate::vcs::against_diff(vcs.as_ref(), rev)));
        let pinned = pinned.map(|(source, pr_head)| pinned_diff(vcs.as_ref(), source, pr_head));
        Ok(Refreshed {
            status,
            model,
            against,
            pinned,
        })
    }

    /// What the repo's git attributes declare about each of `paths`, leaving
    /// out paths they say nothing about. Each lookup is real IO, so we run
    /// this on a worker.
    pub fn compute_declared(
        repo_root: &Path,
        paths: &[String],
    ) -> Result<HashMap<String, crate::classify::Kind>, ReviewError> {
        let vcs = repo::open(repo_root)?;
        Ok(paths
            .iter()
            .filter_map(|path| {
                let rel = Path::new(path);
                let kind = crate::classify::declared(|name| vcs.attr(rel, name))?;
                Some((path.clone(), kind))
            })
            .collect())
    }

    /// Every requested file's content from the copy `read_first` names, else
    /// the other. A path neither copy has is left out. Runs on a worker.
    pub fn compute_walkthrough_files(
        repo_root: &Path,
        rev: Option<&str>,
        read_first: ReadFirst,
        files: &[String],
    ) -> WalkthroughFiles {
        let vcs = repo::open(repo_root).ok();
        let pin_broken = match (rev, vcs.as_ref()) {
            (Some(rev), Some(vcs)) => vcs.resolve(rev).is_err(),
            (Some(_), None) => true,
            (None, _) => false,
        };
        let rev = (!pin_broken).then_some(rev).flatten();
        let contents = files
            .iter()
            .filter_map(|path| {
                let pinned = || rev.and_then(|rev| vcs.as_ref()?.read_at(rev, path).ok().flatten());
                let worktree = || std::fs::read_to_string(repo_root.join(path)).ok();
                let content = match read_first {
                    ReadFirst::Pin => pinned().or_else(worktree),
                    ReadFirst::Worktree => worktree().or_else(pinned),
                }?;
                Some((path.clone(), content))
            })
            .collect();
        WalkthroughFiles {
            contents,
            pin_broken,
        }
    }

    /// Both sides of a binary file, each from its blob, the new side from the
    /// worktree when the store has no blob for it. We check the size first so
    /// a side over [`MAX_PREVIEW_BYTES`] never loads into memory.
    pub fn compute_binary_sides(
        repo_root: &Path,
        path: &str,
        blobs: &crate::model::BlobIds,
        deleted: bool,
    ) -> BinarySides {
        let vcs = repo::open(repo_root).ok();
        let from_blob = |oid: &Option<String>| {
            let bytes = vcs.as_ref()?.read_blob(oid.as_deref()?).ok()??;
            Some(BinarySide::of(bytes))
        };
        let new = from_blob(&blobs.new).or_else(|| {
            if deleted {
                return None;
            }
            let full = repo_root.join(path);
            let size = std::fs::metadata(&full).ok()?.len();
            if size > MAX_PREVIEW_BYTES {
                return Some(BinarySide::TooLarge(size));
            }
            std::fs::read(full).ok().map(BinarySide::of)
        });
        BinarySides {
            old: from_blob(&blobs.old),
            new,
        }
    }

    /// One file's worktree text and blame, for a worker. A file git cannot
    /// blame comes back with no spans.
    pub fn compute_file(repo_root: &Path, rel: &str) -> Result<FileSnapshot, ReviewError> {
        let vcs = repo::open(repo_root)?;
        let path = Path::new(rel);
        let content = std::fs::read_to_string(repo_root.join(path)).map_err(VcsError::from)?;
        Ok(FileSnapshot {
            path: rel.to_owned(),
            blame: vcs.blame(path).unwrap_or_default(),
            content,
        })
    }

    pub fn install_refresh(&mut self, status: StatusModel, model: DiffModel) {
        self.status = status;
        self.session.reconcile(&model);
        self.model = OnceCell::from(model);
    }

    pub fn save(&self) -> Result<(), ReviewError> {
        store::save(&self.repo_root, &self.session)?;
        Ok(())
    }

    /// Load a source's session into the cache. Call before
    /// [`Review::session_for`].
    pub fn ensure_source(&mut self, source: &ReviewSource) -> Result<(), ReviewError> {
        if matches!(source, ReviewSource::WorkingTree) {
            return Ok(());
        }
        let key = source.key();
        if !self.sources.contains_key(&key) {
            let session = store::load_source(&self.repo_root, source)?;
            self.sources.insert(key, (source.clone(), session));
        }
        Ok(())
    }

    /// Empty for a source not yet [`Review::ensure_source`]d.
    pub fn session_for(&self, source: &ReviewSource) -> &Session {
        match source {
            ReviewSource::WorkingTree => &self.session,
            other => self
                .sources
                .get(&other.key())
                .map_or(&self.empty, |(_, session)| session),
        }
    }

    pub fn session_for_mut(&mut self, source: &ReviewSource) -> &mut Session {
        match source {
            ReviewSource::WorkingTree => &mut self.session,
            other => {
                &mut self
                    .sources
                    .entry(other.key())
                    .or_insert_with(|| (other.clone(), Session::default()))
                    .1
            }
        }
    }

    pub fn save_for(&self, source: &ReviewSource) -> Result<(), ReviewError> {
        store::save_source(&self.repo_root, source, self.session_for(source))?;
        Ok(())
    }

    /// Drop a source's cached session once its file is deleted, so a later
    /// access reloads it.
    pub fn forget_source(&mut self, source: &ReviewSource) {
        self.sources.remove(&source.key());
    }

    /// Every review across all sources, in-memory state overriding disk,
    /// sorted by source key. A review file that fails to parse is skipped.
    pub fn all_reviews(&self) -> Result<Vec<(ReviewSource, Session)>, ReviewError> {
        Ok(self.all_reviews_and_corrupt()?.0)
    }

    /// [`Review::all_reviews`] plus the path of every review file skipped.
    pub fn all_reviews_and_corrupt(&self) -> Result<store::LoadedReviews, ReviewError> {
        let (loaded, corrupt) = store::load_all(&self.repo_root)?;
        let mut by_key: BTreeMap<String, (ReviewSource, Session)> = loaded
            .into_iter()
            .map(|(source, session)| (source.key(), (source, session)))
            .collect();
        by_key.insert(
            ReviewSource::WorkingTree.key(),
            (ReviewSource::WorkingTree, self.session.clone()),
        );
        for (key, (source, session)) in &self.sources {
            by_key.insert(key.clone(), (source.clone(), session.clone()));
        }
        Ok((by_key.into_values().collect(), corrupt))
    }

    /// Swap a previous model back in after a no-op refresh, since it carries
    /// render-time emphasis the rebuilt one lacks.
    pub fn restore_model(&mut self, model: DiffModel) {
        self.model = OnceCell::from(model);
    }

    #[cfg(test)]
    fn model_is_cached(&self) -> bool {
        self.model.get().is_some()
    }
}

#[cfg(test)]
mod tests {
    use crate::repo;

    use super::*;

    #[allow(clippy::expect_used)]
    fn write(root: &std::path::Path, rel: &str, content: &str) {
        std::fs::write(root.join(rel), content).expect("write");
    }

    #[allow(clippy::expect_used)]
    fn commit_all(root: &std::path::Path, message: &str) {
        for args in [&["add", "-A"][..], &["commit", "-q", "-m", message][..]] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        }
    }

    #[allow(clippy::expect_used)]
    fn init_repo(root: &std::path::Path) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "-q"])
            .status()
            .expect("git init");
        assert!(status.success());
    }

    #[test]
    fn open_defers_the_working_model_until_first_access() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);
        write(root, "a.py", "value = old\n");
        commit_all(root, "base");
        write(root, "a.py", "value = new\n");

        let root = repo::discover(root).expect("discover");
        let review = Review::open(&root).expect("open");
        assert!(
            !review.model_is_cached(),
            "open must not compute the working model"
        );
        assert_eq!(review.status.unstaged.files.len(), 1);

        let lazy = review.model().clone();
        assert!(review.model_is_cached(), "access caches the model");
        let eager = review.vcs.working_tree_diff().expect("diff");
        assert_eq!(lazy, eager, "lazy model equals the eager build");
    }

    #[allow(clippy::expect_used)]
    fn git(root: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn against_a_base_branch_shows_committed_and_uncommitted_work() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);
        git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        write(root, "base.txt", "base\n");
        commit_all(root, "base");
        git(root, &["checkout", "-q", "-b", "feature"]);
        write(root, "committed.txt", "landed\n");
        commit_all(root, "feature work");
        git(root, &["checkout", "-q", "main"]);
        write(root, "elsewhere.txt", "not mine\n");
        commit_all(root, "base moved on");
        git(root, &["checkout", "-q", "feature"]);
        write(root, "dirty.txt", "still editing\n");

        let root = repo::discover(root).expect("discover");
        let review = Review::open(&root).expect("open");
        let model = crate::vcs::against_diff(review.vcs.as_ref(), "main").expect("against");
        let paths: Vec<&str> = model.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["committed.txt", "dirty.txt"]);
    }

    #[test]
    fn per_source_sessions_persist_independently_and_aggregate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);
        write(root, "a.py", "value = old\n");
        commit_all(root, "base");
        write(root, "a.py", "value = new\n");

        let root = repo::discover(root).expect("discover");
        let mut review = Review::open(&root).expect("open");

        let commit = crate::source::ReviewSource::commit("deadbeef");
        review.ensure_source(&commit).expect("ensure");
        review
            .session_for_mut(&commit)
            .mark_viewed("a.py", "hash-commit");
        review.save_for(&commit).expect("save commit");
        review.session.mark_viewed("a.py", "hash-working");
        review.save().expect("save working");

        assert!(review.session_for(&commit).is_viewed("a.py", "hash-commit"));
        assert!(!review.session.is_viewed("a.py", "hash-commit"));

        let mut reopened = Review::open(&root).expect("reopen");
        reopened.ensure_source(&commit).expect("ensure");
        assert!(
            reopened
                .session_for(&commit)
                .is_viewed("a.py", "hash-commit")
        );
        assert!(reopened.session.is_viewed("a.py", "hash-working"));

        let all = reopened.all_reviews().expect("all");
        let keys: Vec<String> = all.iter().map(|(s, _)| s.key()).collect();
        assert_eq!(keys, ["commit-deadbeef", "working"]);
    }

    #[test]
    fn compute_walkthrough_files_reads_the_pinned_revision_and_falls_back_for_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);
        write(root, "a.txt", "old\n");
        commit_all(root, "base");
        let root = repo::discover(root).expect("discover");
        let pinned = Review::open(&root)
            .expect("open")
            .vcs
            .resolve("HEAD")
            .expect("resolve");
        write(&root, "a.txt", "new\n");
        write(&root, "b.txt", "worktree only\n");

        let files = ["a.txt".to_owned(), "b.txt".to_owned()];
        let read = Review::compute_walkthrough_files(&root, Some(&pinned), ReadFirst::Pin, &files);
        assert_eq!(
            read.contents.get("a.txt").map(String::as_str),
            Some("old\n"),
            "reads the pinned revision, not the dirty worktree"
        );
        assert_eq!(
            read.contents.get("b.txt").map(String::as_str),
            Some("worktree only\n"),
            "a path the revision never had falls back to the worktree"
        );
        assert!(!read.pin_broken, "the pin itself still resolves");
    }

    #[test]
    fn compute_walkthrough_files_with_no_revision_reads_the_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);
        write(root, "a.txt", "committed\n");
        commit_all(root, "base");
        write(root, "a.txt", "edited\n");
        let root = repo::discover(root).expect("discover");

        let files = ["a.txt".to_owned()];
        let read = Review::compute_walkthrough_files(&root, None, ReadFirst::Pin, &files);
        assert_eq!(
            read.contents.get("a.txt").map(String::as_str),
            Some("edited\n")
        );
        assert!(
            !read.pin_broken,
            "no revision was ever pinned, so nothing is broken"
        );
    }

    #[test]
    fn compute_walkthrough_files_with_an_unresolvable_revision_reports_the_broken_pin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);
        write(root, "a.txt", "edited\n");
        commit_all(root, "base");
        let root = repo::discover(root).expect("discover");

        let files = ["a.txt".to_owned()];
        let read = Review::compute_walkthrough_files(
            &root,
            Some("0000000000000000000000000000000000dead"),
            ReadFirst::Pin,
            &files,
        );
        assert_eq!(
            read.contents.get("a.txt").map(String::as_str),
            Some("edited\n"),
            "still falls back to the worktree"
        );
        assert!(read.pin_broken, "the named revision does not resolve");
    }
}
