mod common;

use std::fmt::Write as _;
use std::path::Path;

use common::Fixture;
use diffler_core::git::GitVcs;
use diffler_core::model::{FileStatus, LineKind};
use diffler_core::vcs::{Vcs, VcsError};

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
fn git_dir_resolves_to_the_dot_git_directory() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    let dir = vcs(&fx).git_dir().expect("git dir");
    assert_eq!(
        dir.canonicalize().expect("canonicalize"),
        fx.root().join(".git").canonicalize().expect("canonicalize")
    );
}

#[test]
fn git_dir_in_a_linked_worktree_resolves_the_gitlink() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    let wt_dir = tempfile::tempdir().expect("tempdir");
    let wt = wt_dir.path().join("wt");
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(fx.root())
        .arg("worktree")
        .arg("add")
        .arg(&wt)
        .output()
        .expect("git worktree add");
    assert!(
        output.status.success(),
        "worktree add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(wt.join(".git").is_file(), "worktree .git is a gitlink file");

    let v = GitVcs::open(&wt).expect("open worktree");
    let dir = v.git_dir().expect("git dir");
    assert!(dir.is_dir(), "resolved gitdir exists: {}", dir.display());
    let expected = fx.root().join(".git/worktrees/wt");
    assert_eq!(
        dir.canonicalize().expect("canonicalize"),
        expected.canonicalize().expect("canonicalize")
    );
}

#[test]
fn context_lines_shrink_hunk_context() {
    let fx = Fixture::new();
    let mut base = String::new();
    for i in 1..=20 {
        writeln!(base, "line {i}").expect("write");
    }
    fx.write("a.txt", &base);
    fx.commit_all("base");
    fx.write("a.txt", &base.replace("line 10\n", "LINE TEN\n"));

    let context = |lines: u32| -> usize {
        let v = GitVcs::open_with_context(fx.root(), lines).expect("open");
        let model = v.working_tree_diff().expect("diff");
        model.files[0].hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Context)
            .count()
    };
    assert_eq!(context(3), 6, "git default: three lines each side");
    assert_eq!(context(1), 2, "context_lines=1 keeps one line each side");
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
fn modified_line_pair_carries_intraline_emphasis() {
    let fx = Fixture::new();
    fx.write("a.py", "value = old_name\nrest = 1\n");
    fx.commit_all("base");
    fx.write("a.py", "value = new_name\nrest = 1\n");
    let model = vcs(&fx).working_tree_diff().expect("diff");
    let lines = &model.files[0].hunks[0].lines;
    let deleted = lines
        .iter()
        .find(|l| l.kind == LineKind::Deleted)
        .expect("deleted line");
    let added = lines
        .iter()
        .find(|l| l.kind == LineKind::Added)
        .expect("added line");
    assert_eq!(deleted.text, "value = old_name");
    assert_eq!(added.text, "value = new_name");
    // substitution: both sides emphasize the swapped word
    assert!(!deleted.emphasis.is_empty());
    assert!(!added.emphasis.is_empty());
    let old_hl: String = deleted
        .emphasis
        .iter()
        .map(|r| &deleted.text[r.clone()])
        .collect();
    let new_hl: String = added
        .emphasis
        .iter()
        .map(|r| &added.text[r.clone()])
        .collect();
    assert_eq!(old_hl, "old");
    assert_eq!(new_hl, "new");
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

#[test]
fn stage_moves_untracked_to_staged() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.write("new.txt", "fresh\n");

    let v = vcs(&fx);
    v.stage(Path::new("new.txt")).expect("stage");

    let st = v.status().expect("status");
    assert!(st.untracked.files.is_empty());
    assert!(st.unstaged.files.is_empty());
    assert_eq!(st.staged.files.len(), 1);
    assert_eq!(st.staged.files[0].path, "new.txt");
    assert_eq!(st.staged.files[0].status, FileStatus::Added);
}

#[test]
fn stage_records_a_worktree_deletion() {
    let fx = Fixture::new();
    fx.write("gone.txt", "bye\n");
    fx.commit_all("base");
    fx.remove("gone.txt");

    let v = vcs(&fx);
    v.stage(Path::new("gone.txt")).expect("stage");

    let st = v.status().expect("status");
    assert!(st.unstaged.files.is_empty());
    assert_eq!(st.staged.files.len(), 1);
    assert_eq!(st.staged.files[0].status, FileStatus::Deleted);
}

#[test]
fn unstage_reverses_stage() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");
    fx.write("a.txt", "ONE\n");

    let v = vcs(&fx);
    v.stage(Path::new("a.txt")).expect("stage");
    assert_eq!(v.status().expect("status").staged.files.len(), 1);

    v.unstage(Path::new("a.txt")).expect("unstage");
    let st = v.status().expect("status");
    assert!(st.staged.files.is_empty());
    assert_eq!(st.unstaged.files.len(), 1);
    assert_eq!(st.unstaged.files[0].path, "a.txt");
}

#[test]
fn stage_one_hunk_of_two_then_unstage_hunk_reverses() {
    let fx = Fixture::new();
    let mut base = String::new();
    for i in 1..=40 {
        writeln!(base, "line {i}").expect("write");
    }
    fx.write("a.txt", &base);
    fx.commit_all("base");
    let edited = base
        .replace("line 5\n", "LINE FIVE\n")
        .replace("line 35\n", "LINE THIRTY-FIVE\n");
    fx.write("a.txt", &edited);

    let v = vcs(&fx);
    let st = v.status().expect("status");
    assert_eq!(st.unstaged.files[0].hunks.len(), 2);
    let first = st.unstaged.files[0].hunks[0].id.clone();

    v.stage_hunk(Path::new("a.txt"), &first)
        .expect("stage hunk");

    let st = v.status().expect("status");
    assert_eq!(st.staged.files.len(), 1);
    assert_eq!(st.staged.files[0].hunks.len(), 1);
    assert_eq!(st.staged.files[0].hunks[0].id, first);
    assert_eq!(st.unstaged.files.len(), 1);
    assert_eq!(st.unstaged.files[0].hunks.len(), 1);

    v.unstage_hunk(Path::new("a.txt"), &first)
        .expect("unstage hunk");

    let st = v.status().expect("status");
    assert!(st.staged.files.is_empty());
    assert_eq!(st.unstaged.files[0].hunks.len(), 2);
}

#[test]
fn stage_hunk_on_untracked_file_stages_it() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.write("new.txt", "alpha\nbeta\n");

    let v = vcs(&fx);
    let st = v.status().expect("status");
    let id = st.untracked.files[0].hunks[0].id.clone();
    v.stage_hunk(Path::new("new.txt"), &id).expect("stage hunk");

    let st = v.status().expect("status");
    assert!(st.untracked.files.is_empty());
    assert!(st.unstaged.files.is_empty());
    assert_eq!(st.staged.files.len(), 1);
    assert_eq!(st.staged.files[0].path, "new.txt");
    assert_eq!(st.staged.files[0].status, FileStatus::Added);
    assert_eq!(
        st.staged.files[0].new_text.as_deref(),
        Some("alpha\nbeta\n")
    );
}

#[test]
fn stage_second_hunk_only_applies_at_its_offset() {
    let fx = Fixture::new();
    let mut base = String::new();
    for i in 1..=40 {
        writeln!(base, "line {i}").expect("write");
    }
    fx.write("a.txt", &base);
    fx.commit_all("base");
    let edited = base
        .replace("line 5\n", "LINE FIVE\n")
        .replace("line 35\n", "LINE THIRTY-FIVE\n");
    fx.write("a.txt", &edited);

    let v = vcs(&fx);
    let st = v.status().expect("status");
    assert_eq!(st.unstaged.files[0].hunks.len(), 2);
    let second = st.unstaged.files[0].hunks[1].id.clone();

    v.stage_hunk(Path::new("a.txt"), &second)
        .expect("stage hunk");

    let st = v.status().expect("status");
    assert_eq!(st.staged.files.len(), 1);
    assert_eq!(st.staged.files[0].hunks.len(), 1);
    assert_eq!(st.staged.files[0].hunks[0].id, second);
    let staged_text = st.staged.files[0].new_text.as_deref().expect("text");
    assert!(staged_text.contains("LINE THIRTY-FIVE"));
    assert!(!staged_text.contains("LINE FIVE"));
    assert_eq!(st.unstaged.files[0].hunks.len(), 1);
}

#[test]
fn stage_hunk_on_file_without_trailing_newline() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\ntwo\n");
    fx.commit_all("base");
    fx.write("a.txt", "one\ntwo\nthree");

    let v = vcs(&fx);
    let st = v.status().expect("status");
    let id = st.unstaged.files[0].hunks[0].id.clone();
    v.stage_hunk(Path::new("a.txt"), &id).expect("stage hunk");

    let st = v.status().expect("status");
    assert!(st.unstaged.files.is_empty());
    assert_eq!(st.staged.files.len(), 1);
    assert_eq!(
        st.staged.files[0].new_text.as_deref(),
        Some("one\ntwo\nthree")
    );
}

#[test]
fn stage_hunk_on_crlf_file() {
    let fx = Fixture::new();
    fx.write("a.txt", "alpha\r\nbeta\r\n");
    fx.commit_all("base");
    fx.write("a.txt", "alpha\r\nBETA\r\n");

    let v = vcs(&fx);
    let st = v.status().expect("status");
    let id = st.unstaged.files[0].hunks[0].id.clone();
    v.stage_hunk(Path::new("a.txt"), &id).expect("stage hunk");

    let st = v.status().expect("status");
    assert!(st.unstaged.files.is_empty());
    assert_eq!(st.staged.files.len(), 1);
    assert_eq!(
        st.staged.files[0].new_text.as_deref(),
        Some("alpha\r\nBETA\r\n")
    );
}

#[test]
fn stage_hunk_of_whole_file_deletion_records_the_delete() {
    let fx = Fixture::new();
    fx.write("gone.txt", "bye\n");
    fx.commit_all("base");
    fx.remove("gone.txt");

    let v = vcs(&fx);
    let st = v.status().expect("status");
    let id = st.unstaged.files[0].hunks[0].id.clone();
    v.stage_hunk(Path::new("gone.txt"), &id)
        .expect("stage hunk");

    let st = v.status().expect("status");
    assert!(st.unstaged.files.is_empty());
    assert_eq!(st.staged.files.len(), 1);
    assert_eq!(st.staged.files[0].status, FileStatus::Deleted);
    // the fixture handle caches the index; reload to see what apply wrote
    let mut index = fx.repo.index().expect("index");
    index.read(true).expect("reload");
    assert!(index.get_path(Path::new("gone.txt"), 0).is_none());
}

#[test]
fn unstage_hunk_of_staged_new_file_returns_it_to_untracked() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.write("new.txt", "fresh\n");

    let v = vcs(&fx);
    v.stage(Path::new("new.txt")).expect("stage");
    let st = v.status().expect("status");
    assert_eq!(st.staged.files[0].status, FileStatus::Added);
    let id = st.staged.files[0].hunks[0].id.clone();

    v.unstage_hunk(Path::new("new.txt"), &id)
        .expect("unstage hunk");

    let st = v.status().expect("status");
    assert!(st.staged.files.is_empty(), "no phantom staged entry");
    assert!(st.unstaged.files.is_empty());
    assert_eq!(st.untracked.files.len(), 1);
    assert_eq!(st.untracked.files[0].path, "new.txt");
    // the fixture handle caches the index; reload to see what apply wrote
    let mut index = fx.repo.index().expect("index");
    index.read(true).expect("reload");
    assert!(index.get_path(Path::new("new.txt"), 0).is_none());
}

#[test]
fn stage_hunk_with_stale_id_is_rejected() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");
    fx.write("a.txt", "ONE\n");

    let v = vcs(&fx);
    let stale = diffler_core::model::HunkId("0000000000000000000000000000000000000000".into());
    let err = v.stage_hunk(Path::new("a.txt"), &stale).expect_err("stale");
    assert!(matches!(err, VcsError::Rejected(_)));
}

#[test]
fn discard_restores_modified_file() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");
    fx.write("a.txt", "CHANGED\n");

    let v = vcs(&fx);
    v.discard(Path::new("a.txt")).expect("discard");

    let content = std::fs::read_to_string(fx.root().join("a.txt")).expect("read");
    assert_eq!(content, "one\n");
    assert!(v.working_tree_diff().expect("diff").files.is_empty());
}

#[test]
fn discard_leaves_file_clean_under_autocrlf() {
    // a Windows user with core.autocrlf=true: checkout smudges LF->CRLF, so the
    // file on disk differs in size from the stored blob. discard must still
    // leave the file reported as clean (matching `git status`), not phantom-dirty.
    let fx = Fixture::new();
    {
        let mut config = fx.repo.config().expect("config");
        config.set_str("core.autocrlf", "true").expect("autocrlf");
        config.set_str("core.eol", "crlf").expect("eol");
    }
    fx.write("a.txt", "one\ntwo\nthree\n");
    fx.commit_all("base");
    fx.write("a.txt", "one\nCHANGED\nthree\n");

    let v = vcs(&fx);
    v.discard(Path::new("a.txt")).expect("discard");

    assert!(
        v.working_tree_diff().expect("diff").files.is_empty(),
        "discard left a phantom modification under autocrlf"
    );
    let status = v.status().expect("status");
    assert_eq!(status.unstaged.files.len(), 0, "phantom unstaged file");
}

#[test]
fn discard_deletes_untracked_file() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.write("junk.txt", "scratch\n");

    let v = vcs(&fx);
    v.discard(Path::new("junk.txt")).expect("discard");
    assert!(!fx.root().join("junk.txt").exists());
}

#[test]
fn discard_of_staged_file_is_rejected() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");
    fx.write("a.txt", "STAGED\n");

    let v = vcs(&fx);
    v.stage(Path::new("a.txt")).expect("stage");
    let err = v.discard(Path::new("a.txt")).expect_err("rejected");
    assert!(matches!(err, VcsError::Rejected(_)));
    let content = std::fs::read_to_string(fx.root().join("a.txt")).expect("read");
    assert_eq!(content, "STAGED\n");
}

#[test]
fn commit_creates_commit_and_clears_staged() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");
    fx.write("a.txt", "two\n");

    let v = vcs(&fx);
    v.stage(Path::new("a.txt")).expect("stage");
    let oid = v.commit("change a").expect("commit");
    assert_eq!(oid.len(), 40);

    assert!(v.status().expect("status").staged.files.is_empty());
    let entries = v.log(10).expect("log");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].subject, "change a");
    assert_eq!(entries[0].oid, oid);
}

#[test]
fn empty_commit_message_is_rejected() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");

    let v = vcs(&fx);
    let err = v.commit("  \n").expect_err("rejected");
    assert!(matches!(err, VcsError::Rejected(_)));
}

#[test]
fn branch_create_checkout_and_delete() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");

    let v = vcs(&fx);
    let original = v.head().expect("head").branch.expect("on a branch");

    v.create_branch("feature", true).expect("create");
    assert_eq!(v.head().expect("head").branch.as_deref(), Some("feature"));

    let err = v.delete_branch("feature").expect_err("current branch");
    assert!(matches!(err, VcsError::Rejected(_)));

    let branches = v.branches().expect("branches");
    let feature = branches
        .iter()
        .find(|b| b.name == "feature")
        .expect("feature listed");
    assert!(feature.is_head);

    v.checkout(&original).expect("checkout");
    assert_eq!(v.head().expect("head").branch, Some(original));
    v.delete_branch("feature").expect("delete");
    assert!(
        !v.branches()
            .expect("branches")
            .iter()
            .any(|b| b.name == "feature")
    );
}

#[test]
fn create_branch_without_checkout_keeps_head() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");

    let v = vcs(&fx);
    let original = v.head().expect("head").branch;
    v.create_branch("idle", false).expect("create");
    assert_eq!(v.head().expect("head").branch, original);
    assert!(
        v.branches()
            .expect("branches")
            .iter()
            .any(|b| b.name == "idle")
    );
}
