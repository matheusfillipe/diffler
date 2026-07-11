// fixture helpers run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]
// shared across integration-test binaries that each use a different subset
#![allow(dead_code)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;

/// A throwaway git repo with helpers to commit and mutate files.
pub(crate) struct Fixture {
    pub dir: TempDir,
    pub repo: git2::Repository,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init");
        let mut config = repo.config().expect("config");
        config.set_str("user.name", "test").expect("config");
        config.set_str("user.email", "test@test").expect("config");
        // pin line endings so checkout restores exact bytes across platforms
        config.set_str("core.autocrlf", "false").expect("config");
        config.set_str("core.eol", "lf").expect("config");
        drop(config);
        Self { dir, repo }
    }

    pub(crate) fn write(&self, rel: &str, content: &str) {
        let path = self.dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, content).expect("write");
    }

    pub(crate) fn remove(&self, rel: &str) {
        fs::remove_file(self.dir.path().join(rel)).expect("remove");
    }

    pub(crate) fn commit_all(&self, message: &str) {
        let mut index = self.repo.index().expect("index");
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .expect("add");
        index.write().expect("index write");
        let tree_id = index.write_tree().expect("tree");
        let tree = self.repo.find_tree(tree_id).expect("find tree");
        let sig = self.repo.signature().expect("sig");
        let parent = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        self.repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .expect("commit");
    }

    pub(crate) fn stage(&self, rel: &str) {
        let mut index = self.repo.index().expect("index");
        index.add_path(Path::new(rel)).expect("add");
        index.write().expect("index write");
    }

    /// Create a branch at HEAD without checking it out.
    pub(crate) fn branch(&self, name: &str) {
        let head = self
            .repo
            .head()
            .expect("head")
            .peel_to_commit()
            .expect("commit");
        self.repo.branch(name, &head, false).expect("branch");
    }

    pub(crate) fn checkout(&self, name: &str) {
        self.repo
            .set_head(&format!("refs/heads/{name}"))
            .expect("set head");
        let mut cb = git2::build::CheckoutBuilder::new();
        cb.force();
        self.repo.checkout_head(Some(&mut cb)).expect("checkout");
    }

    pub(crate) fn root(&self) -> &Path {
        self.dir.path()
    }
}
