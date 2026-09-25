//! Repository discovery and backend selection.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::diffalgo::DiffSettings;
use crate::git::GitVcs;
use crate::jj::JjVcs;
use crate::vcs::{Vcs, VcsError};

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("not a git repository (or any parent): {0}")]
    NotFound(PathBuf),
    #[error("repository has no working directory: {0}")]
    Bare(PathBuf),
    #[error(
        "jj repo at {0} is not colocated with git; run `jj git colocation enable` there to use diffler"
    )]
    JjNotColocated(PathBuf),
    #[error(transparent)]
    Git(#[from] git2::Error),
}

/// Discover the repository containing `path` and return its working directory root.
/// A `.jj` directory with no colocated `.git` is a distinct, more actionable
/// failure than a plain "not a repository": jj found, git missing.
pub fn discover(path: &Path) -> Result<PathBuf, RepoError> {
    let repo = match git2::Repository::discover(path) {
        Ok(repo) => repo,
        Err(err) if err.code() == git2::ErrorCode::NotFound => {
            return Err(find_uncolocated_jj(path).map_or_else(
                || RepoError::NotFound(path.to_path_buf()),
                RepoError::JjNotColocated,
            ));
        }
        Err(err) => return Err(RepoError::Git(err)),
    };
    repo.workdir()
        .map(Path::to_path_buf)
        .ok_or_else(|| RepoError::Bare(path.to_path_buf()))
}

/// Walk `path` and its ancestors for a `.jj` directory, for the discovery
/// error message when git2 found no `.git` at all.
fn find_uncolocated_jj(path: &Path) -> Option<PathBuf> {
    let mut dir = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };
    while let Some(candidate) = dir {
        if candidate.join(".jj").is_dir() {
            return Some(candidate.to_path_buf());
        }
        dir = candidate.parent();
    }
    None
}

/// Open the right backend for `root`, as [`discover`] resolved it, at
/// [`DiffSettings::default`]: [`JjVcs`] when a `.jj` directory sits beside
/// `.git` (a colocated jj repo), plain [`GitVcs`] otherwise.
pub fn open(root: &Path) -> Result<Box<dyn Vcs>, VcsError> {
    open_with_settings(root, &DiffSettings::default())
}

/// [`open`] with the line-diff context, algorithm and indent heuristic the
/// review diffs with.
pub fn open_with_settings(root: &Path, settings: &DiffSettings) -> Result<Box<dyn Vcs>, VcsError> {
    if root.join(".jj").is_dir() {
        Ok(Box::new(JjVcs::open_with_settings(root, settings)?))
    } else {
        Ok(Box::new(GitVcs::open_with_settings(root, settings)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_this_repository() {
        let here = std::env::current_dir().expect("cwd");
        let root = discover(&here).expect("repo root");
        assert!(root.join(".git").exists());
    }

    #[test]
    fn fails_outside_a_repository() {
        // discover() walks all ancestors, so this test needs a dir whose
        // ancestors are repo-free; tempdirs satisfy that on CI runners
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(discover(dir.path()), Err(RepoError::NotFound(_))));
    }

    #[test]
    fn bare_repository_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init_bare(dir.path()).expect("init bare");
        assert!(matches!(discover(dir.path()), Err(RepoError::Bare(_))));
    }
}
