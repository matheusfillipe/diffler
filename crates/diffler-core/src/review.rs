//! Facade tying the VCS backend, session, and store together: the one
//! entry point the TUI and MCP layers consume.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::git::GitVcs;
use crate::model::DiffModel;
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

/// The result of [`Review::compute_walkthrough_files`]: every file it could
/// read, plus whether the revision it was asked to pin to still resolves.
#[derive(Debug, Default)]
pub struct WalkthroughFiles {
    pub contents: HashMap<String, String>,
    /// A `rev` was named but no longer resolves (a squash, a rebase, a gc):
    /// every file fell back to the worktree, and the reader is looking at
    /// live code believing it is pinned. `false` when nothing was pinned at
    /// all, which is not broken, just untracked.
    pub pin_broken: bool,
}

/// One landed off-thread refresh.
#[derive(Debug)]
pub struct Refreshed {
    pub status: StatusModel,
    pub model: DiffModel,
    /// The [`ReviewSource::Against`] rev the caller asked about and its
    /// recomputed diff. The rev rides along so a review swapped while the
    /// worker ran can ignore an answer meant for the previous one.
    pub against: Option<(String, Result<DiffModel, VcsError>)>,
}

pub struct Review {
    pub repo_root: PathBuf,
    pub vcs: Box<dyn Vcs>,
    pub status: StatusModel,
    /// HEAD vs workdir+index including untracked: the review view. Computed
    /// lazily on first [`Review::model`] access: the status screen is the
    /// initial view and needs no working diff up front.
    model: OnceCell<DiffModel>,
    /// The working-tree review session, the default view.
    pub session: Session,
    /// Lazily-loaded sessions for non-working sources (commits, ranges), keyed
    /// by [`ReviewSource::key`].
    sources: HashMap<String, (ReviewSource, Session)>,
    /// Returned by [`Review::session_for`] for a source that has no review yet.
    empty: Session,
}

impl Review {
    /// Open the git backend, load the persisted session (if any), and compute
    /// the status sections. The working-tree review diff is deferred until
    /// first [`Review::model`] access. Diffs carry git's default hunk context.
    pub fn open(repo_root: &Path) -> Result<Self, ReviewError> {
        Self::open_with_context(repo_root, crate::git::DEFAULT_CONTEXT_LINES)
    }

    /// Like [`Review::open`] with a custom number of context lines around
    /// diff hunks (config key `ui.context_lines`).
    pub fn open_with_context(repo_root: &Path, context_lines: u32) -> Result<Self, ReviewError> {
        let vcs: Box<dyn Vcs> = Box::new(GitVcs::open_with_context(repo_root, context_lines)?);
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

    /// The working-tree review diff, computed and cached on first access. A
    /// backend error yields an empty diff rather than panicking; the next
    /// [`Review::refresh`] gets another chance to compute it.
    pub fn model(&self) -> &DiffModel {
        self.model
            .get_or_init(|| self.vcs.working_tree_diff().unwrap_or_default())
    }

    /// Mutable view of the working-tree review diff, computing it first if
    /// needed. The TUI uses this to enrich a file with intra-line emphasis
    /// just before rendering it.
    pub fn model_mut(&mut self) -> &mut DiffModel {
        self.model();
        #[allow(clippy::expect_used)]
        self.model.get_mut().expect("model just initialized")
    }

    /// Recompute status + diff (the watcher calls this on changes) and drop
    /// viewed marks for files that changed or left the diff.
    pub fn refresh(&mut self) -> Result<(), ReviewError> {
        self.status = self.vcs.status()?;
        let model = self.vcs.working_tree_diff()?;
        self.install_refresh(self.status.clone(), model);
        Ok(())
    }

    /// Compute a refresh on a separate repo handle, so it can run off the UI
    /// thread; the result is applied later with [`Review::install_refresh`].
    /// `against` recomputes the open three-dot review in the same pass, since
    /// it tracks edits and cannot be pinned like a commit's diff.
    pub fn compute_refresh(
        repo_root: &Path,
        context_lines: u32,
        against: Option<&str>,
    ) -> Result<Refreshed, ReviewError> {
        let vcs = GitVcs::open_with_context(repo_root, context_lines)?;
        let status = vcs.status()?;
        let model = vcs.working_tree_diff()?;
        let against = against.map(|rev| (rev.to_owned(), crate::vcs::against_diff(&vcs, rev)));
        Ok(Refreshed {
            status,
            model,
            against,
        })
    }

    /// What the repo's git attributes declare about each of `paths`, for the
    /// kinds sidebar. One attribute lookup walks the directory chain and the
    /// global attribute files, so this is real IO per path and belongs on a
    /// worker; paths the repo says nothing about are left out.
    pub fn compute_declared(
        repo_root: &Path,
        paths: &[String],
    ) -> Result<HashMap<String, crate::classify::Kind>, ReviewError> {
        let vcs = GitVcs::open(repo_root)?;
        Ok(paths
            .iter()
            .filter_map(|path| {
                let rel = Path::new(path);
                let kind = crate::classify::declared(|name| vcs.attr(rel, name))?;
                Some((path.clone(), kind))
            })
            .collect())
    }

    /// Every requested file's content as the walkthrough's own `rev` recorded
    /// it, falling back to the live worktree for a path that revision has
    /// none of (or when `rev` is `None`, a walkthrough saved before it was
    /// tracked). Opens its own backend so it runs on a worker thread like
    /// [`Review::compute_refresh`]; a path neither the revision nor the
    /// worktree can produce is left out rather than failing the whole read.
    /// `rev` itself can also stop resolving (a squash, a rebase, a gc): that
    /// is distinct from a path merely absent from a revision that still
    /// resolves, so it comes back as `pin_broken` rather than folding into
    /// the same silent worktree fallback.
    pub fn compute_walkthrough_files(
        repo_root: &Path,
        rev: Option<&str>,
        files: &[String],
    ) -> WalkthroughFiles {
        let vcs = GitVcs::open(repo_root).ok();
        let pin_broken = match (rev, vcs.as_ref()) {
            (Some(rev), Some(vcs)) => vcs.resolve(rev).is_err(),
            (Some(_), None) => true,
            (None, _) => false,
        };
        let rev = (!pin_broken).then_some(rev).flatten();
        let contents = files
            .iter()
            .filter_map(|path| {
                let pinned = rev.and_then(|rev| vcs.as_ref()?.read_at(rev, path).ok().flatten());
                let content =
                    pinned.or_else(|| std::fs::read_to_string(repo_root.join(path)).ok())?;
                Some((path.clone(), content))
            })
            .collect();
        WalkthroughFiles {
            contents,
            pin_broken,
        }
    }

    /// One file's worktree text and blame, for the file view. Opens its own
    /// backend so it runs on a worker thread like [`Review::compute_refresh`].
    /// A file git cannot blame (untracked, or newly staged) still loads: it
    /// comes back with text and no spans.
    pub fn compute_file(repo_root: &Path, rel: &str) -> Result<FileSnapshot, ReviewError> {
        let vcs = GitVcs::open(repo_root)?;
        let path = Path::new(rel);
        let content = std::fs::read_to_string(repo_root.join(path)).map_err(VcsError::from)?;
        Ok(FileSnapshot {
            path: rel.to_owned(),
            blame: vcs.blame(path).unwrap_or_default(),
            content,
        })
    }

    /// Swap in freshly computed status + diff and reconcile viewed marks.
    pub fn install_refresh(&mut self, status: StatusModel, model: DiffModel) {
        self.status = status;
        self.session.reconcile(&model);
        self.model = OnceCell::from(model);
    }

    pub fn save(&self) -> Result<(), ReviewError> {
        store::save(&self.repo_root, &self.session)?;
        Ok(())
    }

    /// Load a non-working source's session into the cache if not already there.
    /// Call before reading via [`Review::session_for`] for that source.
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

    /// The session for a source. The working tree is always present; other
    /// sources must be [`Review::ensure_source`]d first, else an empty session
    /// is returned.
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

    /// Forget a non-working source's cached session, after its file is
    /// deleted from disk (e.g. a walkthrough removed for good), so a later
    /// access reloads default state rather than serving stale memory.
    pub fn forget_source(&mut self, source: &ReviewSource) {
        self.sources.remove(&source.key());
    }

    /// Every review across all sources, in-memory state overriding disk, sorted
    /// by source key. Powers the agent-facing aggregate feed. A review file
    /// that fails to parse is skipped rather than failing the whole call; see
    /// [`Review::all_reviews_and_corrupt`] for the list of what was skipped.
    pub fn all_reviews(&self) -> Result<Vec<(ReviewSource, Session)>, ReviewError> {
        Ok(self.all_reviews_and_corrupt()?.0)
    }

    /// [`Review::all_reviews`] plus the path of every review file that failed
    /// to parse and was skipped, for a caller that wants to tell the reader.
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

    /// Swap a previously computed model back in. Used when a refresh proved
    /// a no-op (same fingerprint): the old model carries render-time emphasis
    /// the rebuilt one lacks.
    pub fn restore_model(&mut self, model: DiffModel) {
        self.model = OnceCell::from(model);
    }

    /// Whether the working-tree model has been computed yet.
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
        // the status sections are computed eagerly; the review model is not
        assert!(
            !review.model_is_cached(),
            "open must not compute the working model"
        );
        assert_eq!(review.status.unstaged.files.len(), 1);

        // first access computes it; it matches a fresh working_tree_diff
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
        // main moves on after the fork: three-dot keeps it out of the diff
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

        // the same path means different things per source
        assert!(review.session_for(&commit).is_viewed("a.py", "hash-commit"));
        assert!(!review.session.is_viewed("a.py", "hash-commit"));

        // a fresh open reloads each source from its own file
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

    /// A walkthrough pinned to a revision reads a file as that revision had
    /// it, ignoring a dirty worktree, and falls back to the worktree for a
    /// path the revision never had.
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
        let read = Review::compute_walkthrough_files(&root, Some(&pinned), &files);
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

    /// No `rev` at all (a walkthrough saved before it was tracked) reads the
    /// worktree directly.
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
        let read = Review::compute_walkthrough_files(&root, None, &files);
        assert_eq!(
            read.contents.get("a.txt").map(String::as_str),
            Some("edited\n")
        );
        assert!(
            !read.pin_broken,
            "no revision was ever pinned, so nothing is broken"
        );
    }

    /// A `rev` that no longer resolves (a squash, a rebase, a gc) is a
    /// different fact than a path merely absent from a revision that does
    /// resolve: every file still falls back to the worktree, but `pin_broken`
    /// says so, so the reader is not shown live code believing it is pinned.
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
