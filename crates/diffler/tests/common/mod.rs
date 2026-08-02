//! Shared fixtures for the diffler integration tests.

// fixture helpers run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]
// shared across integration-test binaries that each use a different subset
#![allow(dead_code)]

use std::path::PathBuf;

use tempfile::TempDir;

pub(crate) struct Fixture {
    _dir: TempDir,
    pub root: PathBuf,
}

/// One committed file with an unstaged edit (`41` → `42` on line 2), the
/// same shape the unit-test fixtures use.
pub(crate) fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("fixture");
    std::fs::create_dir(&root).expect("repo dir");
    let repo = diffler_core::test_git::init_repo(&root, Some("main"));

    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    41\n}\n",
    )
    .expect("write");
    let sig = repo.signature().expect("sig");
    diffler_core::test_git::commit_all(&repo, "initial commit", &sig);
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    42\n}\n",
    )
    .expect("write");
    Fixture { _dir: dir, root }
}
