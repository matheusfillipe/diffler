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
        let repo = diffler_core::test_git::init_repo(dir.path(), None);
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
        let sig = self.repo.signature().expect("sig");
        diffler_core::test_git::commit_all(&self.repo, message, &sig);
    }

    /// Commit with an explicit timestamp, for tests that assert on commit time.
    pub(crate) fn commit_all_at(&self, message: &str, unix: i64) {
        let time = git2::Time::new(unix, 0);
        let sig = git2::Signature::new("test", "test@test", &time).expect("sig");
        diffler_core::test_git::commit_all(&self.repo, message, &sig);
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

    pub(crate) fn track(&self, branch: &str, at: &str) {
        diffler_core::test_git::track(&self.repo, branch, at);
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

/// A throwaway repo colocated with jj: a git repo on a pinned branch name
/// (`jj git colocation` imports it as a bookmark of the same name) with one
/// initial commit, then `jj git init --colocate`. jj's own identity is set
/// locally, since it is separate from git's and jj otherwise warns on every
/// write.
pub(crate) struct JjFixture {
    pub git: Fixture,
}

impl JjFixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = diffler_core::test_git::init_repo(dir.path(), Some("main"));
        let git = Fixture { dir, repo };
        git.write("README.md", "hello\n");
        git.commit_all("initial");
        jj_run(git.root(), &["git", "init", "--colocate"]);
        jj_run(
            git.root(),
            &["config", "set", "--repo", "user.name", "reviewer"],
        );
        jj_run(
            git.root(),
            &[
                "config",
                "set",
                "--repo",
                "user.email",
                "reviewer@example.com",
            ],
        );
        Self { git }
    }

    pub(crate) fn root(&self) -> &Path {
        self.git.root()
    }

    pub(crate) fn write(&self, rel: &str, content: &str) {
        self.git.write(rel, content);
    }

    /// Run a jj subcommand directly, for fixture setup the `Vcs` trait has
    /// no method for. Panics on failure so a broken setup step fails at the
    /// call site rather than a confusing assertion later.
    pub(crate) fn jj(&self, args: &[&str]) -> String {
        jj_run(self.root(), args)
    }
}

fn jj_run(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("jj")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run jj");
    assert!(
        output.status.success(),
        "jj {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
