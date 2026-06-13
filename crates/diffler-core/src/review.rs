//! Facade tying the VCS backend, session, and store together: the one
//! entry point the TUI and MCP layers consume.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::git::GitVcs;
use crate::model::DiffModel;
use crate::session::Session;
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
    /// HEAD vs workdir+index including untracked: the review view.
    pub model: DiffModel,
    pub session: Session,
}

impl Review {
    /// Open the git backend, load the persisted session (if any), compute
    /// the status sections + review diff, and reconcile viewed marks.
    /// Diffs carry git's default amount of hunk context.
    pub fn open(repo_root: &Path) -> Result<Self, ReviewError> {
        Self::open_with_context(repo_root, crate::git::DEFAULT_CONTEXT_LINES)
    }

    /// Like [`Review::open`] with a custom number of context lines around
    /// diff hunks (config key `ui.context_lines`).
    pub fn open_with_context(repo_root: &Path, context_lines: u32) -> Result<Self, ReviewError> {
        let vcs: Box<dyn Vcs> = Box::new(GitVcs::open_with_context(repo_root, context_lines)?);
        let status = vcs.status()?;
        let model = vcs.working_tree_diff()?;
        let mut session = store::load(repo_root)?;
        session.reconcile(&model);
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            vcs,
            status,
            model,
            session,
        })
    }

    /// Recompute status + diff (the watcher calls this on changes) and drop
    /// viewed marks for files that changed or left the diff.
    pub fn refresh(&mut self) -> Result<(), ReviewError> {
        self.status = self.vcs.status()?;
        self.model = self.vcs.working_tree_diff()?;
        self.session.reconcile(&self.model);
        Ok(())
    }

    pub fn save(&self) -> Result<(), ReviewError> {
        store::save(&self.repo_root, &self.session)?;
        Ok(())
    }
}
