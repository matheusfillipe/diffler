mod common;

use std::fmt::Write as _;

use common::Fixture;
use diffler_core::git::GitVcs;
use diffler_core::model::{FileStatus, LineKind};
use diffler_core::vcs::Vcs;

// helper fns run outside #[test] fns, where clippy's test allowances don't reach
#[allow(clippy::expect_used)]
fn vcs(fx: &Fixture) -> GitVcs {
    GitVcs::open(fx.root()).expect("open")
}

#[allow(clippy::expect_used)]
fn head_oid(fx: &Fixture) -> String {
    fx.repo
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .id()
        .to_string()
}

#[test]
fn clean_tree_is_empty() {
    let fx = Fixture::new();
    fx.write("a.txt", "hello\n");
    fx.commit_all("base");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    assert!(model.files.is_empty());
}

#[test]
fn modified_file_produces_hunk_with_line_numbers() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\ntwo\nthree\n");
    fx.commit_all("base");
    fx.write("a.txt", "one\nTWO\nthree\n");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    assert_eq!(model.files.len(), 1);
    let file = &model.files[0];
    assert_eq!(file.path, "a.txt");
    assert_eq!(file.status, FileStatus::Modified);
    assert_eq!(file.old_text.as_deref(), Some("one\ntwo\nthree\n"));
    assert_eq!(file.new_text.as_deref(), Some("one\nTWO\nthree\n"));
    assert_eq!(file.hunks.len(), 1);
    let lines = &file.hunks[0].lines;
    let deleted: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == LineKind::Deleted)
        .collect();
    let added: Vec<_> = lines.iter().filter(|l| l.kind == LineKind::Added).collect();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].text, "two");
    assert_eq!(deleted[0].old_no, Some(2));
    assert_eq!(deleted[0].new_no, None);
    assert_eq!(added[0].text, "TWO");
    assert_eq!(added[0].new_no, Some(2));
}

#[test]
fn untracked_file_is_included_as_added_lines() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.write("new.txt", "alpha\nbeta\n");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    let file = model
        .files
        .iter()
        .find(|f| f.path == "new.txt")
        .expect("untracked file present");
    assert_eq!(file.status, FileStatus::Untracked);
    let added: Vec<_> = file.hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(added, vec!["alpha", "beta"]);
}

#[test]
fn deleted_file_reported() {
    let fx = Fixture::new();
    fx.write("gone.txt", "bye\n");
    fx.commit_all("base");
    fx.remove("gone.txt");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    let file = &model.files[0];
    assert_eq!(file.status, FileStatus::Deleted);
    assert!(file.new_text.is_none());
}

#[test]
fn staged_changes_are_included() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");
    fx.write("a.txt", "ONE\n");
    fx.stage("a.txt");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    assert_eq!(model.files.len(), 1);
    assert_eq!(model.files[0].status, FileStatus::Modified);
}

#[test]
fn binary_file_flagged_without_hunks() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    std::fs::write(fx.root().join("blob.bin"), [0u8, 159, 146, 150]).expect("write");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    let file = model
        .files
        .iter()
        .find(|f| f.path == "blob.bin")
        .expect("binary present");
    assert!(file.binary);
    assert!(file.hunks.is_empty());
}

#[test]
fn empty_repo_with_no_head_diffs_against_nothing() {
    let fx = Fixture::new();
    fx.write("first.txt", "hello\n");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    let file = &model.files[0];
    assert_eq!(file.path, "first.txt");
    assert_eq!(file.status, FileStatus::Untracked);
}

#[test]
fn hunk_ids_survive_unrelated_edits() {
    let fx = Fixture::new();
    let mut base = String::new();
    for i in 1..=40 {
        writeln!(base, "line {i}").expect("write");
    }
    fx.write("a.txt", &base);
    fx.commit_all("base");
    let edit_one = base.replace("line 5\n", "LINE FIVE\n");
    fx.write("a.txt", &edit_one);
    let m1 = vcs(&fx).working_tree_diff().expect("diff");
    let id_before = m1.files[0].hunks[0].id.clone();
    // unrelated edit far away creates a second hunk; first hunk id must not move
    let edit_two = edit_one.replace("line 35\n", "LINE THIRTY-FIVE\n");
    fx.write("a.txt", &edit_two);
    let m2 = vcs(&fx).working_tree_diff().expect("diff");
    assert!(m2.files[0].hunks.iter().any(|h| h.id == id_before));
    assert_eq!(m2.files[0].hunks.len(), 2);
}

#[test]
fn commit_diff_shows_only_that_commits_change() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");
    let root_oid = head_oid(&fx);
    fx.write("a.txt", "two\n");
    fx.write("b.txt", "other\n");
    fx.commit_all("second");
    let second_oid = head_oid(&fx);

    let v = vcs(&fx);
    let second = v.commit_diff(&second_oid).expect("diff");
    assert_eq!(second.files.len(), 2);
    let a = second
        .files
        .iter()
        .find(|f| f.path == "a.txt")
        .expect("a.txt");
    assert_eq!(a.status, FileStatus::Modified);
    let deleted: Vec<_> = a.hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Deleted)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(deleted, vec!["one"]);

    // root commit diffs against the empty tree
    let root = v.commit_diff(&root_oid).expect("diff");
    assert_eq!(root.files.len(), 1);
    assert_eq!(root.files[0].status, FileStatus::Added);
}

#[test]
fn log_is_newest_first_with_decorations() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("first");
    fx.branch("marker");
    fx.write("a.txt", "two\n");
    fx.commit_all("second");

    let v = vcs(&fx);
    let entries = v.log(10).expect("log");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].subject, "second");
    assert_eq!(entries[1].subject, "first");
    assert_eq!(entries[0].oid7.len(), 7);
    assert!(entries[0].oid.starts_with(&entries[0].oid7));
    assert_eq!(entries[0].author, "test");
    assert!(entries[0].time_unix > 0);

    let branch = v.head().expect("head").branch.expect("on a branch");
    assert!(entries[0].refs.contains(&branch));
    assert!(entries[1].refs.contains(&"marker".to_owned()));

    assert_eq!(v.log(1).expect("log").len(), 1);
}

#[test]
fn log_on_unborn_repo_is_empty() {
    let fx = Fixture::new();
    assert!(vcs(&fx).log(10).expect("log").is_empty());
}

#[test]
fn head_reports_branch_oid_and_subject() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    let info = vcs(&fx).head().expect("head");
    assert!(info.branch.is_some());
    assert_eq!(info.oid7.len(), 7);
    assert_eq!(info.subject, "base");
    assert!(info.upstream.is_none());
}

#[test]
fn head_on_unborn_repo_has_no_oid() {
    let fx = Fixture::new();
    let info = vcs(&fx).head().expect("head");
    assert!(info.oid7.is_empty());
    assert!(info.subject.is_empty());
}

#[test]
fn head_follows_fixture_checkout() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.branch("side");
    fx.checkout("side");
    let info = vcs(&fx).head().expect("head");
    assert_eq!(info.branch.as_deref(), Some("side"));
}

#[test]
fn status_separates_staged_and_unstaged_edits_of_same_file() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\ntwo\n");
    fx.commit_all("base");
    fx.write("a.txt", "ONE\ntwo\n");
    fx.stage("a.txt");
    fx.write("a.txt", "ONE\nTWO\n");

    let st = vcs(&fx).status().expect("status");
    assert!(st.untracked.files.is_empty());

    let staged = &st.staged.files;
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].path, "a.txt");
    let staged_added: Vec<_> = staged[0].hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(staged_added, vec!["ONE"]);

    let unstaged = &st.unstaged.files;
    assert_eq!(unstaged.len(), 1);
    assert_eq!(unstaged[0].path, "a.txt");
    let unstaged_added: Vec<_> = unstaged[0].hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(unstaged_added, vec!["TWO"]);
}

#[test]
fn status_puts_untracked_in_its_own_section() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.write("new.txt", "fresh\n");

    let st = vcs(&fx).status().expect("status");
    assert_eq!(st.untracked.files.len(), 1);
    assert_eq!(st.untracked.files[0].path, "new.txt");
    assert_eq!(st.untracked.files[0].status, FileStatus::Untracked);
    assert!(st.unstaged.files.is_empty());
    assert!(st.staged.files.is_empty());
}

#[test]
fn branches_lists_locals_with_head_marker() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.branch("other");

    let v = vcs(&fx);
    let head_branch = v.head().expect("head").branch.expect("on a branch");
    let branches = v.branches().expect("branches");
    assert_eq!(branches.len(), 2);
    let current = branches
        .iter()
        .find(|b| b.name == head_branch)
        .expect("current listed");
    assert!(current.is_head);
    let other = branches
        .iter()
        .find(|b| b.name == "other")
        .expect("other listed");
    assert!(!other.is_head);
}
