//! Shared fixtures for the diffler integration tests.

// fixture helpers run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]
// shared across integration-test binaries that each use a different subset
#![allow(dead_code)]

use std::path::{Path, PathBuf};

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
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = git2::Repository::init_opts(&root, &options).expect("init");
    let mut config = repo.config().expect("config");
    config.set_str("user.name", "test").expect("config");
    config.set_str("user.email", "test@test").expect("config");

    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    41\n}\n",
    )
    .expect("write");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("src/lib.rs")).expect("add");
    index.write().expect("index write");
    let tree_id = index.write_tree().expect("tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let sig = git2::Signature::now("test", "test@test").expect("sig");
    repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
        .expect("commit");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    42\n}\n",
    )
    .expect("write");
    Fixture { _dir: dir, root }
}
