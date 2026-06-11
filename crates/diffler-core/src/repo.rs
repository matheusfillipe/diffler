//! Repository discovery.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("not a git repository (or any parent): {0}")]
    NotFound(PathBuf),
    #[error("repository has no working directory: {0}")]
    Bare(PathBuf),
    #[error(transparent)]
    Git(#[from] git2::Error),
}

/// Discover the repository containing `path` and return its working directory root.
pub fn discover(path: &Path) -> Result<PathBuf, RepoError> {
    let repo = git2::Repository::discover(path).map_err(|err| {
        if err.code() == git2::ErrorCode::NotFound {
            RepoError::NotFound(path.to_path_buf())
        } else {
            RepoError::Git(err)
        }
    })?;
    repo.workdir()
        .map(Path::to_path_buf)
        .ok_or_else(|| RepoError::Bare(path.to_path_buf()))
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
