//! Git2 repo scaffolding shared by fixture builders in this crate's and
//! diffler's tests. Feature-gated so it never ships in a normal build.

// fixture helpers run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]

use std::path::Path;

pub fn init_repo(root: &Path, initial_branch: Option<&str>) -> git2::Repository {
    let repo = match initial_branch {
        Some(branch) => {
            let mut options = git2::RepositoryInitOptions::new();
            options.initial_head(branch);
            git2::Repository::init_opts(root, &options).expect("init")
        }
        None => git2::Repository::init(root).expect("init"),
    };
    let mut config = repo.config().expect("config");
    config.set_str("user.name", "test").expect("config");
    config.set_str("user.email", "test@test").expect("config");
    // pin line endings so checkout restores exact bytes across platforms
    config.set_str("core.autocrlf", "false").expect("config");
    config.set_str("core.eol", "lf").expect("config");
    repo
}

pub fn commit_all(repo: &git2::Repository, message: &str, sig: &git2::Signature<'_>) {
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .expect("add");
    index.write().expect("index write");
    let tree_id = index.write_tree().expect("tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    repo.commit(Some("HEAD"), sig, sig, message, &tree, &parents)
        .expect("commit");
}
