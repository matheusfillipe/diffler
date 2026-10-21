//! Integration tests for the jj backend against a real colocated repo.
//! Needs `jj` on PATH.

mod common;

use std::path::Path;

use common::{JjFixture, JjRemote, JjStoreFixture};
use diffler_core::jj::JjVcs;
use diffler_core::model::FileStatus;
use diffler_core::vcs::{ChangeOp, NetworkOp, PullOutcome, PullTarget, Vcs, VcsError, WorktreeOp};

const LOG_LIMIT: usize = 64;

#[allow(clippy::expect_used)]
fn vcs(fx: &JjFixture) -> JjVcs {
    JjVcs::open(fx.root()).expect("open")
}

#[test]
fn jj_has_no_index_or_native_git_checkout() {
    let fx = JjFixture::new();
    let vcs = vcs(&fx);
    assert!(!vcs.has_index());
    assert!(!vcs.native_git_checkout());
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
    assert_eq!(
        v.network_argv(NetworkOp::Fetch).expect("fetch"),
        ["jj", "git", "fetch"]
    );
    assert_eq!(
        v.network_argv(NetworkOp::FetchAll).expect("fetch --all"),
        ["jj", "git", "fetch", "--all-remotes"]
    );
}

#[test]
fn network_argv_declines_gits_pull_modes() {
    let fx = JjFixture::new();
    let v = vcs(&fx);
    for op in [NetworkOp::PullRebase, NetworkOp::PullMerge] {
        assert!(matches!(v.network_argv(op), Err(VcsError::Rejected(_))));
    }
    assert_eq!(
        v.network_argv(NetworkOp::Pull).expect("pull"),
        ["jj", "git", "fetch"]
    );
    assert_eq!(
        v.network_argv(NetworkOp::PullFrom {
            remote: "upstream".into(),
            branch: "main".into(),
        })
        .expect("pull from"),
        ["jj", "git", "fetch", "--remote=upstream"]
    );
}

/// `argv` as [`Vcs::network_argv`] built it, run the way the app runs it.
fn run_network(root: &Path, argv: &[String]) {
    assert_eq!(argv.first().map(String::as_str), Some("jj"));
    let jj_args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    diffler_core::test_git::jj(root, &jj_args);
}

#[test]
fn push_sends_the_nearest_local_bookmark_and_tracks_it() {
    let fx = JjFixture::new();
    let remote = JjRemote::new();
    fx.jj(&["git", "remote", "add", "origin", &remote.url()]);
    fx.write("a.txt", "a\n");
    let oid = vcs(&fx).commit("add a").expect("commit");
    let v = vcs(&fx);
    fx.jj(&["bookmark", "create", "feature", "-r", "@-"]);
    let argv = v.network_argv(NetworkOp::Push).expect("push");
    assert_eq!(
        argv,
        ["jj", "git", "push", "--bookmark=exact:\"feature\""],
        "a bookmark made after the handle opened counts"
    );
    run_network(fx.root(), &argv);
    assert_eq!(remote.tip("feature"), Some(oid));
    let tracked = fx.jj(&["bookmark", "list", "--tracked", "feature"]);
    assert!(tracked.contains("@origin"), "{tracked}");
}

#[test]
fn push_and_set_upstream_names_the_remote() {
    let fx = JjFixture::new();
    let v = vcs(&fx);
    assert_eq!(
        v.network_argv(NetworkOp::PushSetUpstream {
            remote: "fork".into()
        })
        .expect("push -u"),
        [
            "jj",
            "git",
            "push",
            "--remote=fork",
            "--bookmark=exact:\"main\""
        ]
    );
}

#[test]
fn push_with_no_bookmark_has_jj_name_the_parent_change() {
    let fx = JjStoreFixture::new();
    let remote = JjRemote::new();
    fx.jj(&["git", "remote", "add", "origin", &remote.url()]);
    let v = store_vcs(&fx);
    let argv = v.network_argv(NetworkOp::Push).expect("push");
    assert_eq!(argv, ["jj", "git", "push", "--change=@-"]);
    run_network(fx.root(), &argv);
    let branches = remote.branches();
    assert!(
        matches!(branches.as_slice(), [name] if name.starts_with("push-")),
        "{branches:?}"
    );
}

#[test]
fn push_asks_to_move_a_bookmark_left_behind() {
    let fx = JjFixture::new();
    fx.write("a.txt", "a\n");
    vcs(&fx).commit("add a").expect("commit");
    assert!(matches!(
        vcs(&fx).network_argv(NetworkOp::Push),
        Err(VcsError::Rejected(message)) if message.contains("jj bookmark move main --to @-")
    ));
}

#[test]
fn push_on_top_of_someone_elses_remote_bookmark_has_nothing_to_send() {
    let (fx, remote) = tracking_fixture();
    remote.advance("main", "theirs.txt", "theirs\n");
    remote.branch_off("main", "other", "other.txt");
    fx.jj(&["git", "fetch"]);
    fx.jj(&["new", "other@origin"]);
    fx.write("mine.txt", "mine\n");
    assert!(matches!(
        vcs(&fx).network_argv(NetworkOp::Push),
        Err(VcsError::Rejected(message)) if message.contains("nothing new to push")
    ));
}

/// A colocated repo whose `main` tracks `origin`'s, with `@` holding
/// uncommitted work on top of it.
fn tracking_fixture() -> (JjFixture, JjRemote) {
    let fx = JjFixture::new();
    let remote = JjRemote::new();
    fx.jj(&["git", "remote", "add", "origin", &remote.url()]);
    fx.jj(&["git", "push", "--bookmark", "main"]);
    fx.write("mine.txt", "mine\n");
    (fx, remote)
}

#[test]
fn pull_rebases_the_working_copy_onto_the_moved_remote_bookmark() {
    let (fx, remote) = tracking_fixture();
    remote.advance("main", "theirs.txt", "theirs\n");
    let v = vcs(&fx);
    let target = v.pull_target().expect("target").expect("a bookmark");
    assert_eq!(
        target,
        PullTarget {
            bookmark: "main".into(),
            remote: Some("origin".into())
        }
    );
    let fetch = NetworkOp::PullFrom {
        remote: "origin".into(),
        branch: "main".into(),
    };
    run_network(fx.root(), &v.network_argv(fetch).expect("pull"));
    assert_eq!(
        v.rebase_onto_remote("main", "origin").expect("rebase"),
        PullOutcome::Rebased {
            onto: "main@origin".into(),
            conflicts: 0,
            bookmark_conflicted: false,
        }
    );
    let v = vcs(&fx);
    assert_eq!(v.head().expect("head").subject, "remote change");
    assert!(fx.root().join("theirs.txt").exists());
    let paths: Vec<String> = v
        .working_tree_diff()
        .expect("diff")
        .files
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(paths, ["mine.txt"], "@ keeps its own work");
    assert_eq!(
        v.rebase_onto_remote("main", "origin").expect("again"),
        PullOutcome::UpToDate {
            onto: "main@origin".into()
        }
    );
}

#[test]
fn pull_says_so_when_the_remote_deleted_the_bookmark() {
    let (fx, remote) = tracking_fixture();
    fx.jj(&["bookmark", "create", "feature", "-r", "@-"]);
    fx.jj(&["git", "push", "--bookmark", "feature"]);
    remote.delete("feature");
    fx.jj(&["git", "fetch"]);
    assert_eq!(
        vcs(&fx)
            .rebase_onto_remote("feature", "origin")
            .expect("rebase"),
        PullOutcome::Gone {
            onto: "feature@origin".into()
        }
    );
}

#[test]
fn pull_reports_conflicts_the_rebase_left() {
    let (fx, remote) = tracking_fixture();
    fx.write("README.md", "ours\n");
    remote.advance("main", "README.md", "theirs\n");
    let v = vcs(&fx);
    run_network(fx.root(), &v.network_argv(NetworkOp::Pull).expect("pull"));
    assert_eq!(
        v.rebase_onto_remote("main", "origin").expect("rebase"),
        PullOutcome::Rebased {
            onto: "main@origin".into(),
            conflicts: 1,
            bookmark_conflicted: false,
        }
    );
}

#[test]
fn a_bookmark_that_tracks_nothing_has_no_remote_to_pull() {
    let fx = JjFixture::new();
    assert_eq!(
        vcs(&fx).pull_target().expect("target"),
        Some(PullTarget {
            bookmark: "main".into(),
            remote: None
        })
    );
}

#[test]
fn commit_paths_commits_only_the_named_files() {
    for colocated in [true, false] {
        let (root, _keep): (std::path::PathBuf, Box<dyn std::any::Any>) = if colocated {
            let fx = JjFixture::new();
            (fx.root().to_owned(), Box::new(fx))
        } else {
            let fx = JjStoreFixture::new();
            (fx.root().to_owned(), Box::new(fx))
        };
        for name in ["picked.txt", "paren(x).txt", "left.txt"] {
            std::fs::write(root.join(name), "new\n").expect("write");
        }
        let v = JjVcs::open(&root).expect("open");
        let oid = v
            .commit_paths(
                "pick two",
                &["picked.txt".to_owned(), "paren(x).txt".to_owned()],
            )
            .expect("commit");
        let head = v.head().expect("head");
        assert_eq!(head.subject, "pick two", "colocated: {colocated}");
        assert_eq!(head.oid7, oid.get(..7).expect("short"));
        assert!(v.read_at(&oid, "picked.txt").expect("read").is_some());
        assert!(v.read_at(&oid, "paren(x).txt").expect("read").is_some());
        assert!(v.read_at(&oid, "left.txt").expect("read").is_none());
        let left: Vec<String> = v
            .working_tree_diff()
            .expect("diff")
            .files
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(left, ["left.txt"], "colocated: {colocated}");
    }
}

#[test]
fn commit_paths_takes_a_folder_whole() {
    let fx = JjFixture::new();
    fx.write("dir/x.txt", "x\n");
    fx.write("dir/deep/y.txt", "y\n");
    fx.write("dirt.txt", "z\n");
    let v = vcs(&fx);
    let oid = v
        .commit_paths("the folder", &["dir".to_owned()])
        .expect("commit");
    assert!(v.read_at(&oid, "dir/x.txt").expect("read").is_some());
    assert!(v.read_at(&oid, "dir/deep/y.txt").expect("read").is_some());
    assert!(v.read_at(&oid, "dirt.txt").expect("read").is_none());
}

/// The subject of `@-` and `@`, read fresh from jj.
#[allow(clippy::expect_used)]
fn subjects(root: &Path) -> (String, String) {
    let v = JjVcs::open(root).expect("open");
    let view = v.read_jj_view(LOG_LIMIT).expect("view").expect("jj repo");
    (
        v.head().expect("head").subject,
        view.change.expect("@").description,
    )
}

#[test]
fn change_verbs_move_and_rewrite_changes() {
    let fx = JjFixture::new();
    fx.write("a.txt", "a\n");
    let added = vcs(&fx).commit("add a").expect("commit");
    let initial = vcs(&fx).resolve("main").expect("main");

    vcs(&fx)
        .change(&ChangeOp::Describe {
            rev: added.clone(),
            message: "-renamed".into(),
        })
        .expect("describe");
    assert_eq!(subjects(fx.root()).0, "-renamed");
    vcs(&fx)
        .change(&ChangeOp::Describe {
            rev: "@".into(),
            message: "wip".into(),
        })
        .expect("describe @");
    assert_eq!(subjects(fx.root()).1, "wip");

    let renamed = vcs(&fx).resolve("@-").expect("@-");
    vcs(&fx).change(&ChangeOp::Edit(renamed)).expect("edit");
    assert_eq!(subjects(fx.root()), ("initial".into(), "-renamed".into()));

    vcs(&fx).change(&ChangeOp::New(initial)).expect("new");
    assert_eq!(subjects(fx.root()), ("initial".into(), String::new()));
    assert!(!fx.root().join("a.txt").exists());
}

#[test]
fn a_verb_on_a_rewritten_commit_acts_on_its_change_without_diverging() {
    let fx = JjFixture::new();
    fx.write("a.txt", "a\n");
    let stale = vcs(&fx).commit("add a").expect("commit");
    vcs(&fx)
        .change(&ChangeOp::Describe {
            rev: stale.clone(),
            message: "renamed".into(),
        })
        .expect("describe");
    vcs(&fx).change(&ChangeOp::Edit(stale)).expect("edit");
    assert_eq!(subjects(fx.root()).1, "renamed");
    let divergent = fx.jj(&["log", "--no-graph", "-r", "divergent()", "-T", "commit_id"]);
    assert!(divergent.is_empty(), "{divergent}");
}

#[test]
fn an_empty_description_clears_it() {
    let fx = JjFixture::new();
    fx.jj(&["describe", "--message=draft"]);
    vcs(&fx)
        .change(&ChangeOp::Describe {
            rev: "@".into(),
            message: String::new(),
        })
        .expect("describe");
    assert_eq!(subjects(fx.root()).1, "");
}

#[test]
fn squash_keeps_both_descriptions() {
    let fx = JjFixture::new();
    fx.write("a.txt", "a\n");
    vcs(&fx).commit("add a").expect("commit");
    fx.write("b.txt", "b\n");
    fx.jj(&["describe", "--message=add b"]);
    vcs(&fx)
        .change(&ChangeOp::Squash("@".into()))
        .expect("squash");
    let v = vcs(&fx);
    let head = v.resolve("HEAD").expect("head");
    assert_eq!(
        v.commit_message(&head).expect("message"),
        "add a\n\nadd b\n"
    );
    assert!(v.read_at("HEAD", "b.txt").expect("read").is_some());
}

#[test]
fn squash_of_an_undescribed_change_keeps_the_parents_description() {
    let fx = JjFixture::new();
    fx.write("a.txt", "a\n");
    vcs(&fx).commit("add a").expect("commit");
    fx.write("b.txt", "b\n");
    vcs(&fx)
        .change(&ChangeOp::Squash("@".into()))
        .expect("squash");
    let v = vcs(&fx);
    let head = v.resolve("HEAD").expect("head");
    assert_eq!(v.commit_message(&head).expect("message"), "add a\n");
}

#[test]
fn undo_refuses_to_drop_edits_jj_would_snapshot() {
    let fx = JjFixture::new();
    fx.write("a.txt", "a\n");
    vcs(&fx).commit("add a").expect("commit");
    fx.write("d.txt", "dirty\n");
    assert!(matches!(
        vcs(&fx).change(&ChangeOp::Undo),
        Err(VcsError::Rejected(message)) if message.contains("would drop them")
    ));
    assert!(fx.root().join("d.txt").exists());
    vcs(&fx)
        .change(&ChangeOp::Describe {
            rev: "@".into(),
            message: "wip".into(),
        })
        .expect("describe");
    vcs(&fx).change(&ChangeOp::Undo).expect("undo the describe");
    assert_eq!(subjects(fx.root()), ("add a".into(), String::new()));
    assert!(fx.root().join("d.txt").exists(), "the edits survive");
    fx.write("e.txt", "more\n");
    assert!(matches!(
        vcs(&fx).change(&ChangeOp::Redo),
        Err(VcsError::Rejected(message)) if message.contains("would drop them")
    ));
    assert!(fx.root().join("e.txt").exists());
}

#[test]
fn abandon_drops_a_change_and_undo_redo_walk_the_operation_log() {
    let fx = JjFixture::new();
    fx.write("a.txt", "a\n");
    let added = vcs(&fx).commit("add a").expect("commit");
    vcs(&fx).change(&ChangeOp::Abandon(added)).expect("abandon");
    assert_eq!(subjects(fx.root()).0, "initial");
    vcs(&fx).change(&ChangeOp::Undo).expect("undo");
    assert_eq!(subjects(fx.root()).0, "add a");
    vcs(&fx).change(&ChangeOp::Redo).expect("redo");
    assert_eq!(subjects(fx.root()).0, "initial");
}

#[test]
fn change_verbs_work_in_a_repo_without_git() {
    let fx = JjStoreFixture::new();
    fx.write("b.txt", "b\n");
    store_vcs(&fx).commit("add b").expect("commit");
    let initial = store_vcs(&fx).resolve("@--").expect("@--");
    store_vcs(&fx).change(&ChangeOp::New(initial)).expect("new");
    assert_eq!(subjects(fx.root()).0, "initial");
    store_vcs(&fx).change(&ChangeOp::Undo).expect("undo");
    assert_eq!(subjects(fx.root()).0, "add b");
}

#[test]
fn jj_refuses_to_rewrite_an_immutable_change() {
    let (fx, _remote) = tracking_fixture();
    let main = vcs(&fx).resolve("main").expect("main");
    assert!(matches!(
        vcs(&fx).change(&ChangeOp::Abandon(main)),
        Err(VcsError::Rejected(message)) if message.contains("immutable")
    ));
}

#[test]
fn the_view_names_the_working_copy_commit() {
    let fx = JjFixture::new();
    let v = vcs(&fx);
    let change = v
        .read_jj_view(LOG_LIMIT)
        .expect("view")
        .and_then(|view| view.change)
        .expect("@");
    assert_eq!(change.commit_id, v.resolve("@").expect("@"));
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
    let entries = vcs(&fx).log(10, false).expect("log");
    let subjects: Vec<&str> = entries.iter().map(|e| e.subject.as_str()).collect();
    assert!(subjects.contains(&"add a"), "{subjects:?}");
    assert!(subjects.contains(&"initial"), "{subjects:?}");
}

#[allow(clippy::expect_used)]
fn store_vcs(fx: &JjStoreFixture) -> JjVcs {
    JjVcs::open(fx.root()).expect("open")
}

#[test]
fn a_repo_without_git_is_discovered_and_opened_as_jj() {
    let fx = JjStoreFixture::new();
    let root = diffler_core::repo::discover(fx.root()).expect("discover");
    assert_eq!(root, fx.root());
    assert!(!diffler_core::repo::open(&root).expect("open").has_index());
}

#[test]
fn a_repo_without_git_diffs_the_working_copy_against_the_parent() {
    let fx = JjStoreFixture::new();
    fx.write("a.txt", "two\n");
    fx.write("b.txt", "new\n");
    fx.write("nested/c.txt", "deep\n");
    fx.write(".gitignore", "*.log\n");
    fx.write("noise.log", "ignored\n");
    let vcs = store_vcs(&fx);
    let model = vcs.working_tree_diff().expect("diff");
    let files: Vec<(&str, FileStatus)> = model
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.status))
        .collect();
    assert_eq!(
        files,
        [
            (".gitignore", FileStatus::Added),
            ("a.txt", FileStatus::Modified),
            ("b.txt", FileStatus::Added),
            ("nested/c.txt", FileStatus::Added),
        ],
        "jj tracks every new file it does not ignore"
    );
    let status = vcs.status().expect("status");
    assert_eq!(status.staged.files.len(), 4);
    assert!(status.unstaged.files.is_empty() && status.untracked.files.is_empty());
}

#[test]
fn a_repo_without_git_is_clean_after_a_commit() {
    let fx = JjStoreFixture::new();
    fx.write("b.txt", "new\n");
    let vcs = store_vcs(&fx);
    let oid = vcs.commit("add b").expect("commit");
    assert!(vcs.status().expect("status").staged.files.is_empty());
    let head = vcs.head().expect("head");
    assert_eq!(head.oid7, oid.get(..7).expect("short"));
    assert_eq!(head.subject, "add b");
    assert_eq!(head.branch, None);
}

#[test]
fn a_repo_without_git_reads_head_as_the_parent_change() {
    let fx = JjStoreFixture::new();
    let vcs = store_vcs(&fx);
    let head = vcs.head().expect("head");
    assert_eq!(head.subject, "initial");
    assert_eq!(
        vcs.resolve("HEAD").expect("resolve"),
        vcs.resolve("@-").expect("@-")
    );
    assert_eq!(
        vcs.read_at("HEAD", "a.txt").expect("read").as_deref(),
        Some("one\n")
    );
    let tracked: Vec<String> = vcs
        .tracked_files()
        .expect("tracked")
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert_eq!(tracked, ["README.md", "a.txt"]);
    fx.jj(&["new"]);
    assert_eq!(vcs.head().expect("head").subject, "initial");
    vcs.read_jj_view(LOG_LIMIT).expect("view");
    assert_eq!(
        vcs.head().expect("head").subject,
        "",
        "HEAD follows @- once the handle reads jj's view"
    );
}

#[test]
fn a_merge_working_copy_bases_on_its_first_parent() {
    let fx = JjStoreFixture::new();
    fx.jj(&["bookmark", "create", "first", "-r", "@-"]);
    fx.jj(&["new", "root()", "--message=side"]);
    fx.write("side.txt", "side\n");
    fx.jj(&["bookmark", "create", "second", "-r", "@"]);
    fx.jj(&["new", "first", "second"]);
    let vcs = store_vcs(&fx);
    assert_eq!(vcs.head().expect("head").subject, "initial");
    assert_eq!(
        vcs.resolve("HEAD").expect("HEAD"),
        vcs.resolve("first").expect("first")
    );
    assert!(vcs.resolve("@-").is_err(), "@- names both parents");
}

#[test]
fn a_repo_on_jjs_root_commit_has_an_empty_head() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = std::process::Command::new("jj")
        .current_dir(dir.path())
        .args(["git", "init", "--no-colocate"])
        .output()
        .expect("jj git init");
    assert!(output.status.success(), "{output:?}");
    let vcs = JjVcs::open(dir.path()).expect("open");
    let head = vcs.head().expect("head");
    assert!(head.oid7.is_empty());
    assert_eq!(head.branch, None);
    assert!(vcs.log(10, false).expect("log").is_empty());
}

#[test]
fn a_repo_without_git_logs_change_ids_and_jjs_empty_description_wording() {
    let fx = JjStoreFixture::new();
    fx.jj(&["new"]);
    let vcs = store_vcs(&fx);
    vcs.read_jj_view(LOG_LIMIT).expect("view");
    let entries = vcs.log(10, false).expect("log");
    let subjects: Vec<&str> = entries.iter().map(|e| e.subject.as_str()).collect();
    assert_eq!(subjects, ["(no description set)", "initial"]);
    assert!(
        entries
            .iter()
            .all(|e| e.change_id.as_ref().is_some_and(|id| id.len() == 8))
    );
}

#[test]
fn revsets_resolve_in_a_repo_without_git() {
    let fx = JjStoreFixture::new();
    fx.jj(&["bookmark", "create", "topic", "-r", "@-"]);
    let vcs = store_vcs(&fx);
    vcs.read_jj_view(LOG_LIMIT).expect("view");
    let parent = vcs.resolve("@-").expect("@-");
    assert_eq!(
        vcs.resolve("topic").expect("a bookmark only jj knows"),
        parent
    );
    let change = vcs.log(1, false).expect("log")[0]
        .change_id
        .clone()
        .expect("change id");
    assert_eq!(vcs.resolve(&change).expect("change id"), parent);
    assert_eq!(vcs.resolve(&parent).expect("oid"), parent);
    assert!(matches!(
        vcs.resolve("@ | @-"),
        Err(VcsError::Rejected(message)) if message.contains("several commits")
    ));
    assert!(matches!(
        vcs.resolve("none()"),
        Err(VcsError::Rejected(message)) if message.contains("matches no commit")
    ));
    assert!(vcs.resolve("nonesuch").is_err());
}

#[test]
fn a_repo_without_git_lists_jjs_bookmarks_as_branches_and_log_refs() {
    let fx = JjStoreFixture::new();
    fx.jj(&["bookmark", "create", "topic", "-r", "@-"]);
    let vcs = store_vcs(&fx);
    vcs.read_jj_view(LOG_LIMIT).expect("view");
    let branches = vcs.branches().expect("branches");
    assert_eq!(
        branches
            .iter()
            .map(|b| (b.name.as_str(), b.is_head))
            .collect::<Vec<_>>(),
        [("topic", true)]
    );
    assert!(branches[0].tip_unix > 0);
    assert_eq!(vcs.all_branches().expect("all"), ["topic"]);
    let entries = vcs.log(10, false).expect("log");
    assert_eq!(entries[0].refs, ["topic"], "{entries:?}");
}

#[test]
fn a_secondary_workspace_names_the_main_one() {
    let fx = JjStoreFixture::new();
    let workspace = fx.root().join("second");
    fx.jj(&[
        "workspace",
        "add",
        "--name=second",
        workspace.to_str().expect("utf-8 path"),
    ]);
    let Err(VcsError::Rejected(message)) = JjVcs::open(&workspace) else {
        panic!("a secondary workspace must be refused");
    };
    assert!(message.contains("main jj workspace"), "{message}");
}

#[test]
fn trunk_resolves_in_a_repo_without_git() {
    let fx = JjStoreFixture::new();
    fx.jj(&["bookmark", "create", "main", "-r", "@-"]);
    fx.jj(&[
        "config",
        "set",
        "--repo",
        "revset-aliases.\"trunk()\"",
        "main",
    ]);
    let vcs = store_vcs(&fx);
    assert_eq!(
        vcs.resolve("trunk()").expect("trunk"),
        vcs.resolve("@-").expect("@-")
    );
}

#[test]
fn the_working_change_names_its_id_description_and_nearest_bookmark() {
    let fx = JjStoreFixture::new();
    fx.jj(&["bookmark", "create", "topic", "-r", "@-"]);
    let vcs = store_vcs(&fx);
    let change = || {
        vcs.read_jj_view(LOG_LIMIT)
            .expect("view")
            .and_then(|view| view.change)
            .expect("jj repo")
    };
    let first = change();
    assert_eq!(first.change_id.len(), 8);
    assert_eq!(first.description, "");
    assert_eq!(
        first.bookmark.as_deref(),
        Some("topic"),
        "ancestor bookmark"
    );
    fx.jj(&["describe", "--message=wip"]);
    fx.jj(&["bookmark", "create", "here", "-r", "@"]);
    let change = change();
    assert_eq!(change.description, "wip");
    assert_eq!(
        change.bookmark.as_deref(),
        Some("here"),
        "bookmark on @ wins"
    );
}

#[test]
fn a_colocated_repo_logs_change_ids_and_resolves_revsets() {
    let fx = JjFixture::new();
    fx.jj(&["bookmark", "create", "topic", "-r", "@-"]);
    let vcs = vcs(&fx);
    let view = vcs.read_jj_view(LOG_LIMIT).expect("view").expect("jj repo");
    assert!(view.change.is_some());
    assert_eq!(
        view.base,
        Some(vcs.resolve("HEAD").expect("head")),
        "jj's base is where colocated git's HEAD sits"
    );
    let entries = vcs.log(10, false).expect("log");
    assert!(entries.iter().all(|e| e.change_id.is_some()), "{entries:?}");
    assert_eq!(
        vcs.resolve("topic").expect("bookmark"),
        vcs.resolve("HEAD").expect("head")
    );
    assert_eq!(
        vcs.resolve("@-").expect("@-"),
        vcs.resolve("HEAD").expect("head")
    );
}

#[test]
fn git_syntax_resolves_through_git_in_a_colocated_repo() {
    let fx = JjFixture::new();
    for n in 1..=3 {
        fx.write("a.txt", &format!("v{n}\n"));
        vcs(&fx).commit(&format!("change {n}")).expect("commit");
    }
    fx.jj(&["bookmark", "set", "main", "-r", "@-", "--allow-backwards"]);
    let vcs = vcs(&fx);
    let git = |rev: &str| {
        let object = fx.git.repo.revparse_single(rev).expect("git rev");
        object.peel_to_commit().expect("commit").id().to_string()
    };
    assert_eq!(vcs.resolve("main~2").expect("main~2"), git("main~2"));
    assert_eq!(vcs.resolve("HEAD^").expect("HEAD^"), git("HEAD^"));
    let first = git("main~2");
    fx.git
        .repo
        .reference("refs/heads/main", first.parse().expect("oid"), true, "test")
        .expect("move main");
    assert_eq!(
        vcs.resolve("main").expect("main"),
        first,
        "git's own ref wins over jj's stale import"
    );
}

#[test]
fn what_git_cannot_read_resolves_as_a_jj_revset_in_a_colocated_repo() {
    let fx = JjFixture::new();
    let vcs = vcs(&fx);
    let view = vcs.read_jj_view(LOG_LIMIT).expect("view").expect("jj repo");
    let change = view.change.expect("@").change_id;
    let working_copy = vcs.resolve("@").expect("@");
    assert_eq!(vcs.resolve(&change).expect("change id"), working_copy);
    assert_ne!(working_copy, vcs.resolve("HEAD").expect("HEAD"));
}

#[test]
fn a_failing_jj_leaves_the_git_reads_of_a_refresh_standing() {
    let fx = JjFixture::new();
    fx.write("a.txt", "changed\n");
    std::fs::write(fx.root().join(".jj/repo/store/type"), "nonesuch").expect("break jj");
    let refreshed = diffler_core::review::Review::compute_refresh(
        fx.root(),
        &diffler_core::diffalgo::DiffSettings::default(),
        None,
        None,
        diffler_core::review::CommitLimits {
            recent: 10,
            unpushed: 10,
            log: LOG_LIMIT,
        },
    )
    .expect("refresh");
    assert!(refreshed.jj.is_err());
    assert_eq!(refreshed.head.expect("head").subject, "initial");
    let recent = refreshed.commits.expect("commits").recent;
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].change_id, None);
    assert_eq!(refreshed.model.files.len(), 1);
}

#[test]
fn repo_open_picks_the_jj_backend_for_a_colocated_root() {
    let fx = JjFixture::new();
    let root = diffler_core::repo::discover(fx.root()).expect("discover");
    let opened = diffler_core::repo::open(&root).expect("open");
    assert!(!opened.has_index(), "the jj backend was picked, not git's");
}

#[test]
fn a_repo_without_git_names_commits_by_jjs_bookmarks_over_stale_git_refs() {
    let fx = JjStoreFixture::new();
    fx.jj(&["bookmark", "create", "main", "-r", "@-"]);
    fx.jj(&["git", "export"]);
    fx.write("a.txt", "two\n");
    fx.jj(&["commit", "--message=second"]);
    fx.jj(&["bookmark", "set", "main", "-r", "@-"]);
    let vcs = store_vcs(&fx);
    assert_eq!(
        vcs.resolve("main").expect("main"),
        vcs.resolve("@-").expect("@-")
    );
}

#[test]
fn a_repo_without_git_applies_git_ancestry_to_jjs_bookmarks() {
    let fx = JjStoreFixture::new();
    let initial = store_vcs(&fx).resolve("@-").expect("initial");
    fx.write("a.txt", "two\n");
    fx.jj(&["commit", "--message=second"]);
    fx.jj(&["bookmark", "create", "topic", "-r", "@-"]);
    let vcs = store_vcs(&fx);
    assert_eq!(vcs.resolve("topic~1").expect("topic~1"), initial);
    assert_eq!(vcs.resolve("topic^").expect("topic^"), initial);
    assert!(vcs.resolve("topic~5").is_err(), "past the first commit");
    assert!(matches!(
        vcs.resolve("nonesuch~1"),
        Err(VcsError::Rejected(message)) if message.contains("name one before `~1`")
    ));
}

#[test]
fn a_bookmark_on_jjs_root_commit_leaves_the_log_and_resolve_working() {
    let fx = JjStoreFixture::new();
    fx.jj(&["bookmark", "create", "rooted", "-r", "root()"]);
    let vcs = store_vcs(&fx);
    vcs.read_jj_view(LOG_LIMIT).expect("view");
    let entries = vcs.log(10, true).expect("all-branches log");
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        vcs.resolve("root()"),
        Err(VcsError::Rejected(message)) if message.contains("root commit")
    ));
}

#[test]
fn a_repo_without_git_keeps_its_last_base_and_fails_a_fresh_read_when_jj_fails() {
    let fx = JjStoreFixture::new();
    let vcs = store_vcs(&fx);
    vcs.read_jj_view(LOG_LIMIT).expect("view");
    std::fs::write(fx.root().join(".jj/repo/store/type"), "nonesuch").expect("break jj");
    assert!(vcs.read_jj_view(LOG_LIMIT).is_err());
    assert_eq!(vcs.head().expect("head").subject, "initial");
    assert!(
        store_vcs(&fx).head().is_err(),
        "an unborn base would mislead"
    );
    assert!(
        diffler_core::review::Review::compute_refresh(
            fx.root(),
            &diffler_core::diffalgo::DiffSettings::default(),
            None,
            None,
            diffler_core::review::CommitLimits {
                recent: 10,
                unpushed: 10,
                log: LOG_LIMIT,
            },
        )
        .is_err()
    );
}

#[test]
fn worktrees_list_jj_workspaces_and_forget_one_by_name() {
    let fx = JjFixture::new();
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let second = elsewhere.path().join("second");
    fx.jj(&[
        "workspace",
        "add",
        "--name=second",
        &second.display().to_string(),
    ]);
    let worktrees = vcs(&fx).worktrees().expect("worktrees");
    let names: Vec<(&str, bool)> = worktrees
        .iter()
        .map(|w| (w.name.as_str(), w.current))
        .collect();
    assert_eq!(names, [("default", true), ("second", false)]);
    assert_eq!(
        vcs(&fx)
            .worktree_argv(&WorktreeOp::Remove {
                worktree: worktrees[1].clone(),
                force: false,
            })
            .expect("forget"),
        ["jj", "workspace", "forget", "second"]
    );
}
