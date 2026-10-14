//! Integration tests for the jj backend against a real colocated repo.
//! Needs `jj` on PATH.

mod common;

use std::path::Path;

use common::{JjFixture, JjStoreFixture};
use diffler_core::jj::JjVcs;
use diffler_core::model::FileStatus;
use diffler_core::vcs::{NetworkOp, Vcs, VcsError};

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
fn network_argv_rejects_every_push_and_pull_variant() {
    let fx = JjFixture::new();
    let v = vcs(&fx);
    for op in [
        NetworkOp::Push,
        NetworkOp::PushSetUpstream {
            remote: "origin".into(),
        },
        NetworkOp::Pull,
        NetworkOp::PullFrom {
            remote: "origin".into(),
            branch: "main".into(),
        },
        NetworkOp::PullRebase,
        NetworkOp::PullMerge,
    ] {
        assert!(
            matches!(v.network_argv(op), Err(VcsError::Rejected(_))),
            "push/pull must decline in a jj repo"
        );
    }
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
