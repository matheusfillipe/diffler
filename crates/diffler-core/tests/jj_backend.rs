//! Integration tests for the jj backend against a real colocated repo.
//! Needs `jj` on PATH.

mod common;

use std::path::Path;

use common::JjFixture;
use diffler_core::jj::JjVcs;
use diffler_core::vcs::{NetworkOp, Vcs, VcsError, VcsKind};

#[allow(clippy::expect_used)]
fn vcs(fx: &JjFixture) -> JjVcs {
    JjVcs::open(fx.root()).expect("open")
}

#[test]
fn vcs_kind_is_jj() {
    let fx = JjFixture::new();
    assert_eq!(vcs(&fx).vcs_kind(), VcsKind::Jj);
}

#[test]
fn status_folds_everything_into_one_section() {
    let fx = JjFixture::new();
    fx.write("a.txt", "changed\n");
    fx.write("b.txt", "new\n");
    let status = vcs(&fx).status().expect("status");
    assert!(status.untracked.files.is_empty());
    assert!(status.unstaged.files.is_empty());
    let paths: Vec<&str> = status
        .staged
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(paths, ["a.txt", "b.txt"]);
}

#[test]
fn status_is_clean_right_after_a_commit() {
    let fx = JjFixture::new();
    fx.write("a.txt", "content\n");
    vcs(&fx).commit("add a").expect("commit");
    let status = vcs(&fx).status().expect("status");
    assert!(status.staged.files.is_empty(), "@ is fresh and empty");
}

#[test]
fn commit_commits_the_working_copy_and_returns_its_oid() {
    let fx = JjFixture::new();
    fx.write("a.txt", "content\n");
    let oid = vcs(&fx).commit("add a").expect("commit");
    let head = vcs(&fx).head().expect("head");
    assert_eq!(head.oid7, oid.get(..7).expect("short"));
    assert_eq!(head.subject, "add a");
    let content = vcs(&fx)
        .read_at(&oid, "a.txt")
        .expect("read")
        .expect("present");
    assert_eq!(content, "content\n");
}

#[test]
fn a_message_starting_with_a_dash_is_kept_as_the_message() {
    let fx = JjFixture::new();
    fx.write("a.txt", "content\n");
    vcs(&fx).commit("-v1 notes").expect("commit");
    assert_eq!(vcs(&fx).head().expect("head").subject, "-v1 notes");
    vcs(&fx).amend(Some("-v2"), false).expect("reword");
    assert_eq!(vcs(&fx).head().expect("head").subject, "-v2");
}

#[test]
fn commit_with_empty_message_is_rejected() {
    let fx = JjFixture::new();
    fx.write("a.txt", "content\n");
    assert!(matches!(vcs(&fx).commit("   "), Err(VcsError::Rejected(_))));
}

#[test]
fn extend_folds_new_content_keeping_the_parent_message() {
    let fx = JjFixture::new();
    fx.write("a.txt", "v1\n");
    vcs(&fx).commit("add a").expect("commit");
    fx.write("b.txt", "v1\n");
    let oid = vcs(&fx).amend(None, true).expect("extend");
    let head = vcs(&fx).head().expect("head");
    assert_eq!(head.oid7, oid.get(..7).expect("short"));
    assert_eq!(head.subject, "add a", "extend keeps the parent's message");
    assert!(vcs(&fx).read_at(&oid, "b.txt").expect("read").is_some());
}

#[test]
fn extend_over_a_described_working_copy_keeps_the_parent_message() {
    let fx = JjFixture::new();
    fx.write("a.txt", "v1\n");
    vcs(&fx).commit("add a").expect("commit");
    fx.write("b.txt", "v1\n");
    fx.jj(&["describe", "--message=wip"]);
    vcs(&fx).amend(None, true).expect("extend");
    assert_eq!(vcs(&fx).head().expect("head").subject, "add a");
}

#[test]
fn amend_with_a_message_changes_both_content_and_message() {
    let fx = JjFixture::new();
    fx.write("a.txt", "v1\n");
    vcs(&fx).commit("add a").expect("commit");
    fx.write("b.txt", "v1\n");
    vcs(&fx).amend(Some("add a and b"), true).expect("amend");
    let head = vcs(&fx).head().expect("head");
    assert_eq!(head.subject, "add a and b");
}

#[test]
fn reword_changes_only_the_message() {
    let fx = JjFixture::new();
    fx.write("a.txt", "v1\n");
    let before = vcs(&fx).commit("add a").expect("commit");
    vcs(&fx).amend(Some("renamed"), false).expect("reword");
    let head = vcs(&fx).head().expect("head");
    assert_eq!(head.subject, "renamed");
    let content = vcs(&fx)
        .read_at(&head.oid7, "a.txt")
        .expect("read")
        .expect("present");
    assert_eq!(content, "v1\n", "reword leaves the tree untouched");
    assert_ne!(
        head.oid7,
        before.get(..7).expect("short"),
        "reword rewrites"
    );
}

#[test]
fn amend_with_empty_message_is_rejected() {
    let fx = JjFixture::new();
    fx.write("a.txt", "v1\n");
    vcs(&fx).commit("add a").expect("commit");
    assert!(matches!(
        vcs(&fx).amend(Some("  "), true),
        Err(VcsError::Rejected(_))
    ));
}

#[test]
fn create_branch_labels_the_current_change() {
    let fx = JjFixture::new();
    vcs(&fx).create_branch("feature", false).expect("create");
    let branches = vcs(&fx).branches().expect("branches");
    let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
    assert!(names.contains(&"feature"), "{names:?}");
}

#[test]
fn create_branch_that_already_exists_is_rejected() {
    let fx = JjFixture::new();
    assert!(matches!(
        vcs(&fx).create_branch("main", false),
        Err(VcsError::Rejected(_))
    ));
}

#[test]
fn delete_branch_removes_it() {
    let fx = JjFixture::new();
    vcs(&fx).create_branch("feature", false).expect("create");
    vcs(&fx).delete_branch("feature").expect("delete");
    let names: Vec<String> = vcs(&fx)
        .branches()
        .expect("branches")
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(!names.contains(&"feature".to_owned()), "{names:?}");
}

#[test]
fn checkout_switches_the_working_copy_to_another_branch() {
    let fx = JjFixture::new();
    fx.write("only_on_main.txt", "x\n");
    vcs(&fx).commit("add only_on_main").expect("commit");
    assert!(fx.root().join("only_on_main.txt").exists());

    vcs(&fx).checkout("main").expect("checkout");
    assert!(
        !fx.root().join("only_on_main.txt").exists(),
        "checkout moved the working copy back to main's content"
    );
}

#[test]
fn branch_names_that_read_as_revset_syntax_are_checked_out_and_deleted() {
    let fx = JjFixture::new();
    fx.git.branch("u@v");
    fx.write("later.txt", "x\n");
    vcs(&fx).commit("add later").expect("commit");

    vcs(&fx).checkout("u@v").expect("checkout");
    assert!(!fx.root().join("later.txt").exists());
    vcs(&fx).delete_branch("u@v").expect("delete");
    let names: Vec<String> = vcs(&fx)
        .branches()
        .expect("branches")
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(!names.contains(&"u@v".to_owned()), "{names:?}");
}

#[test]
fn discard_takes_paths_that_read_as_fileset_syntax_or_flags() {
    let fx = JjFixture::new();
    for name in ["paren(x).txt", "-dash.txt", "sp ace.txt"] {
        fx.write(name, "new\n");
        vcs(&fx).discard(Path::new(name)).expect("discard");
        assert!(!fx.root().join(name).exists(), "{name}");
    }
}

#[test]
fn discard_reverts_a_modified_file() {
    let fx = JjFixture::new();
    fx.write("a.txt", "v1\n");
    vcs(&fx).commit("add a").expect("commit");
    fx.write("a.txt", "changed\n");
    vcs(&fx).discard(Path::new("a.txt")).expect("discard");
    let content = std::fs::read_to_string(fx.root().join("a.txt")).expect("read");
    assert_eq!(content, "v1\n");
}

#[test]
fn staging_verbs_are_declined() {
    let fx = JjFixture::new();
    fx.write("a.txt", "v1\n");
    let v = vcs(&fx);
    let hunk = diffler_core::model::HunkId("irrelevant".to_owned());
    assert!(matches!(
        v.stage(Path::new("a.txt")),
        Err(VcsError::Rejected(_))
    ));
    assert!(matches!(v.stage_everything(), Err(VcsError::Rejected(_))));
    assert!(matches!(
        v.stage_hunk(Path::new("a.txt"), &hunk),
        Err(VcsError::Rejected(_))
    ));
    assert!(matches!(
        v.unstage(Path::new("a.txt")),
        Err(VcsError::Rejected(_))
    ));
    assert!(matches!(v.unstage_everything(), Err(VcsError::Rejected(_))));
    assert!(matches!(
        v.unstage_hunk(Path::new("a.txt"), &hunk),
        Err(VcsError::Rejected(_))
    ));
    assert!(matches!(v.stash_push(None), Err(VcsError::Rejected(_))));
    assert!(matches!(v.stash_pop(), Err(VcsError::Rejected(_))));
}

#[test]
fn network_argv_maps_each_op_to_the_jj_cli() {
    let fx = JjFixture::new();
    let v = vcs(&fx);
    assert_eq!(v.network_argv(NetworkOp::Fetch), ["jj", "git", "fetch"]);
    assert_eq!(
        v.network_argv(NetworkOp::FetchAll),
        ["jj", "git", "fetch", "--all-remotes"]
    );
}

#[test]
fn working_tree_diff_reads_through_the_colocated_git_repo() {
    let fx = JjFixture::new();
    fx.write("a.txt", "changed\n");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    let paths: Vec<&str> = model.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["a.txt"]);
}

#[test]
fn log_reads_through_the_colocated_git_repo() {
    let fx = JjFixture::new();
    fx.write("a.txt", "v1\n");
    vcs(&fx).commit("add a").expect("commit");
    let entries = vcs(&fx).log(10).expect("log");
    let subjects: Vec<&str> = entries.iter().map(|e| e.subject.as_str()).collect();
    assert!(subjects.contains(&"add a"), "{subjects:?}");
    assert!(subjects.contains(&"initial"), "{subjects:?}");
}

#[test]
fn opening_a_non_colocated_jj_repo_is_reported_by_discover() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = std::process::Command::new("jj")
        .current_dir(dir.path())
        .args(["git", "init", "--no-colocate"])
        .output()
        .expect("jj git init");
    assert!(output.status.success(), "{output:?}");

    let err = diffler_core::repo::discover(dir.path()).expect_err("not colocated");
    assert!(
        matches!(err, diffler_core::repo::RepoError::JjNotColocated(_)),
        "{err:?}"
    );
}

#[test]
fn repo_open_picks_the_jj_backend_for_a_colocated_root() {
    let fx = JjFixture::new();
    let root = diffler_core::repo::discover(fx.root()).expect("discover");
    let opened =
        diffler_core::repo::open(&root, diffler_core::git::DEFAULT_CONTEXT_LINES).expect("open");
    assert_eq!(opened.vcs_kind(), VcsKind::Jj);
}
