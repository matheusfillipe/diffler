// fixture helpers run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]
// shared across integration-test binaries that each use a different subset
#![allow(dead_code)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;

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

/// A jj-colocated repo on `main` with one initial commit.
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
        diffler_core::test_git::colocate_jj(git.root());
        Self { git }
    }

    pub(crate) fn root(&self) -> &Path {
        self.git.root()
    }

    pub(crate) fn write(&self, rel: &str, content: &str) {
        self.git.write(rel, content);
    }

    pub(crate) fn jj(&self, args: &[&str]) -> String {
        diffler_core::test_git::jj(self.root(), args)
    }
}

/// A bare git remote, plus a jj clone of it that moves its bookmarks the way
/// another contributor would.
pub(crate) struct JjRemote {
    pub dir: TempDir,
}

impl JjRemote {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // jj's clone tracks the remote's HEAD branch, so we pin it to main on
        // every machine

        git2::Repository::init_opts(
            dir.path().join("remote.git"),
            git2::RepositoryInitOptions::new()
                .bare(true)
                .initial_head("main"),
        )
        .expect("bare remote");
        Self { dir }
    }

    pub(crate) fn url(&self) -> String {
        self.dir
            .path()
            .join("remote.git")
            .to_str()
            .expect("utf-8 path")
            .to_owned()
    }

    /// The commit `branch` names on the remote.
    pub(crate) fn tip(&self, branch: &str) -> Option<String> {
        let repo = git2::Repository::open_bare(self.dir.path().join("remote.git")).expect("open");
        let reference = repo.find_reference(&format!("refs/heads/{branch}")).ok()?;
        reference.target().map(|oid| oid.to_string())
    }

    pub(crate) fn branches(&self) -> Vec<String> {
        let repo = git2::Repository::open_bare(self.dir.path().join("remote.git")).expect("open");
        repo.branches(None)
            .expect("branches")
            .filter_map(|branch| branch.ok()?.0.name().ok()?.map(str::to_owned))
            .collect()
    }

    /// Another contributor's clone, fetched fresh.
    fn clone(&self) -> std::path::PathBuf {
        let clone = self.dir.path().join("other");
        let jj = |root: &Path, args: &[&str]| diffler_core::test_git::jj(root, args);
        if !clone.exists() {
            jj(self.dir.path(), &["git", "clone", &self.url(), "other"]);
            jj(
                &clone,
                &["config", "set", "--repo", "user.name", "reviewer"],
            );
            jj(
                &clone,
                &[
                    "config",
                    "set",
                    "--repo",
                    "user.email",
                    "reviewer@example.com",
                ],
            );
        }
        jj(&clone, &["git", "fetch"]);
        clone
    }

    /// Commit `content` to `file` on top of `branch` from another clone and
    /// push it.
    pub(crate) fn advance(&self, branch: &str, file: &str, content: &str) {
        let clone = self.clone();
        let jj = |args: &[&str]| diffler_core::test_git::jj(&clone, args);
        jj(&["new", &format!("{branch}@origin")]);
        fs::write(clone.join(file), content).expect("write");
        jj(&["commit", "--message=remote change"]);
        jj(&["bookmark", "set", branch, "-r", "@-", "--allow-backwards"]);
        jj(&["git", "push", "--bookmark", branch]);
    }

    /// Push a new branch `name` from another clone, one commit on `base`.
    pub(crate) fn branch_off(&self, base: &str, name: &str, file: &str) {
        let clone = self.clone();
        let jj = |args: &[&str]| diffler_core::test_git::jj(&clone, args);
        jj(&["new", &format!("{base}@origin")]);
        fs::write(clone.join(file), "branched\n").expect("write");
        jj(&["commit", "--message=branched change"]);
        jj(&["bookmark", "create", name, "-r", "@-"]);
        jj(&["git", "push", "--bookmark", name]);
    }

    /// Delete `branch` on the remote, as a merged pull request does.
    pub(crate) fn delete(&self, branch: &str) {
        let repo = git2::Repository::open_bare(self.dir.path().join("remote.git")).expect("open");
        repo.find_reference(&format!("refs/heads/{branch}"))
            .expect("branch")
            .delete()
            .expect("delete");
    }
}

/// A jj repo with no `.git`: one commit `initial` holding `README.md` and
/// `a.txt`, an empty `@` on top.
pub(crate) struct JjStoreFixture {
    pub dir: TempDir,
}

impl JjStoreFixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let fx = Self { dir };
        fx.jj(&["git", "init", "--no-colocate"]);
        fx.jj(&["config", "set", "--repo", "user.name", "reviewer"]);
        fx.jj(&[
            "config",
            "set",
            "--repo",
            "user.email",
            "reviewer@example.com",
        ]);
        fx.write("README.md", "hello\n");
        fx.write("a.txt", "one\n");
        fx.jj(&["commit", "--message=initial"]);
        fx
    }

    pub(crate) fn root(&self) -> &Path {
        self.dir.path()
    }

    pub(crate) fn write(&self, rel: &str, content: &str) {
        let path = self.dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, content).expect("write");
    }

    pub(crate) fn jj(&self, args: &[&str]) -> String {
        diffler_core::test_git::jj(self.root(), args)
    }
}
