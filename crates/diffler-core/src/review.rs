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

pub struct Review {
    pub repo_root: PathBuf,
    pub vcs: Box<dyn Vcs>,
    pub status: StatusModel,
    /// HEAD vs workdir+index including untracked: the review view. Computed
    /// lazily on first [`Review::model`] access — the status screen is the
    /// initial view and never needs the whole working diff up front.
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
        self.session.reconcile(&model);
        self.model = OnceCell::from(model);
        Ok(())
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

    /// Every review across all sources, in-memory state overriding disk, sorted
    /// by source key. Powers the agent-facing aggregate feed.
    pub fn all_reviews(&self) -> Result<Vec<(ReviewSource, Session)>, ReviewError> {
        let mut by_key: BTreeMap<String, (ReviewSource, Session)> =
            store::load_all(&self.repo_root)?
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
        Ok(by_key.into_values().collect())
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
}
