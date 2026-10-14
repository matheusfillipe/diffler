//! Tests for the file sidebar: its layouts, folds, kinds and stepping.

use super::*;

#[test]
fn open_starts_on_the_sidebar_at_the_first_file() {
    let fixture = standard_fixture();
    let app = diff_app(&fixture);
    assert_eq!(focus(&app), Pane::List);
    assert_eq!(app.diff.as_ref().unwrap().selected, 0);
}

#[test]
fn list_jk_moves_over_tree_rows_and_files_under_the_cursor_become_selected() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    // standard fixture tree (model order ci.yml, src/lib.rs, todo.md):
    //   0 dir src   1 file lib.rs   2 file ci.yml   3 file todo.md
    assert_eq!(
        tree_kinds(&app),
        vec![
            "dir".to_owned(),
            "file:lib.rs".to_owned(),
            "file:ci.yml".to_owned(),
            "file:todo.md".to_owned(),
        ]
    );
    // the cursor opens on the row of the shown file (ci.yml, model index 0)
    assert_eq!(tree_cursor(&app), 2);
    assert_eq!(selected_path(&app), "ci.yml");
    // gg lands on the src dir row; the pane keeps its last file
    app.handle(key('g'));
    app.handle(key('g'));
    assert_eq!(tree_cursor(&app), 0);
    assert_eq!(selected_path(&app), "ci.yml", "a dir row keeps the pane");
    // move the diff cursor down first, then j onto the lib.rs file row
    // selects it and resets the diff cursor
    enter_diff_pane(&mut app);
    app.handle(key('j'));
    assert!(app.diff.as_ref().unwrap().cursor > 0);
    app.diff.as_mut().unwrap().focus = Pane::List;
    app.handle(key('j'));
    assert_eq!(tree_cursor(&app), 1);
    assert_eq!(selected_path(&app), "src/lib.rs");
    assert_eq!(app.diff.as_ref().unwrap().cursor, 0, "diff cursor reset");
    assert_eq!(app.diff.as_ref().unwrap().scroll, 0);
    // j again advances onto the next file (ci.yml at root)
    app.handle(key('j'));
    assert_eq!(selected_path(&app), "ci.yml");
    // k back onto the lib.rs file row reselects it
    app.handle(key('k'));
    assert_eq!(selected_path(&app), "src/lib.rs");
}

#[test]
fn the_default_sidebar_layout_is_a_tree_with_dir_rows() {
    let fixture = standard_fixture();
    let app = diff_app(&fixture);
    // the default keeps the collapsible tree: a src dir row precedes lib.rs
    assert_eq!(
        tree_kinds(&app),
        vec![
            "dir".to_owned(),
            "file:lib.rs".to_owned(),
            "file:ci.yml".to_owned(),
            "file:todo.md".to_owned(),
        ]
    );
}

#[test]
fn gg_and_g_jump_to_the_first_and_last_visible_row() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    let last = tree_row_count(&app) - 1;
    app.handle(key('G'));
    assert_eq!(tree_cursor(&app), last);
    // the last visible row is a file (todo.md), so it is selected
    assert_eq!(selected_path(&app), "todo.md");
    app.handle(key('g'));
    app.handle(key('g'));
    assert_eq!(tree_cursor(&app), 0, "back to the first visible row");
}

#[test]
fn folding_a_dir_hides_its_subtree_and_unfolding_restores_it() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    // cursor onto the src dir row (the first visible row)
    app.handle(key('g'));
    app.handle(key('g'));
    assert_eq!(tree_kinds(&app).len(), 4);
    // za folds it: lib.rs disappears, the dir row stays
    app.handle(key('z'));
    app.handle(key('a'));
    assert_eq!(
        tree_kinds(&app),
        vec![
            "dir".to_owned(),
            "file:ci.yml".to_owned(),
            "file:todo.md".to_owned(),
        ],
        "folded src/ hides lib.rs"
    );
    // <cr> on the dir row also toggles: this unfolds it again
    app.handle(key('\n'));
    assert_eq!(
        tree_kinds(&app).len(),
        4,
        "unfolded src/ shows lib.rs again"
    );
}

#[test]
fn cr_on_a_file_row_focuses_the_diff_pane() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    // ci.yml's row is under the cursor at open; <cr> focuses the pane
    assert!(matches!(
        tree_kinds(&app).get(tree_cursor(&app)).map(String::as_str),
        Some("file:ci.yml")
    ));
    app.handle(key('\n'));
    assert_eq!(focus(&app), Pane::Diff);
}

#[test]
fn h_and_l_focus_the_panes_and_clamp() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    assert_eq!(focus(&app), Pane::List);
    app.handle(key('l'));
    assert_eq!(focus(&app), Pane::Diff);
    app.handle(key('l'));
    assert_eq!(
        focus(&app),
        Pane::Diff,
        "repeats stay on the diff, no cycle"
    );
    app.handle(key('h'));
    assert_eq!(focus(&app), Pane::List);
    app.handle(key('h'));
    assert_eq!(
        focus(&app),
        Pane::List,
        "repeats stay on the sidebar, no cycle"
    );
}

#[test]
fn arrow_keys_focus_the_panes_like_h_and_l() {
    use crossterm::event::KeyCode;
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    app.handle(code_key(KeyCode::Right));
    assert_eq!(focus(&app), Pane::Diff);
    app.handle(code_key(KeyCode::Left));
    assert_eq!(focus(&app), Pane::List);
}

#[test]
fn tab_on_a_file_at_the_repo_root_says_there_is_nothing_to_fold() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    // the tree opens on ci.yml, which sits in no folder
    let before = tree_kinds(&app);

    app.handle(key('\t'));

    assert_eq!(tree_kinds(&app), before, "nothing folded");
    assert!(
        app.message
            .as_ref()
            .is_some_and(|m| m.text.contains("nothing to fold")),
        "{:?}",
        app.message
    );
}

#[test]
fn tab_folds_the_directory_the_sidebar_cursor_sits_in() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    // onto src/lib.rs, a file inside the only folder
    app.handle(key('k'));
    let file = selected_path(&app);
    assert!(tree_kinds(&app).contains(&"file:lib.rs".to_owned()));

    app.handle(key('\t'));

    assert!(
        !tree_kinds(&app).contains(&"file:lib.rs".to_owned()),
        "tab folds the folder away: {:?}",
        tree_kinds(&app)
    );
    assert_eq!(selected_path(&app), file, "and steps to no other file");
    // the cursor rode up to the folder row, so the same key opens it again
    app.handle(key('\t'));
    assert!(tree_kinds(&app).contains(&"file:lib.rs".to_owned()));
}

#[test]
fn cr_from_the_list_focuses_the_diff_and_cr_back_returns() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    app.handle(key('\n'));
    assert_eq!(focus(&app), Pane::Diff);
    app.handle(key('\n'));
    assert_eq!(focus(&app), Pane::List);
}

#[test]
fn ctrl_n_switches_the_selected_file_from_the_diff_pane_keeping_focus() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    let first = selected_path(&app);
    app.handle(ctrl_key('n'));
    assert_eq!(focus(&app), Pane::Diff, "focus stays on the diff");
    assert_ne!(selected_path(&app), first, "selection advanced");
    app.handle(ctrl_key('p'));
    assert_eq!(selected_path(&app), first);
}

#[test]
fn ctrl_n_and_ctrl_p_walk_only_file_rows_skipping_directories() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    // tree: dir src, lib.rs, ci.yml, todo.md. c-n/c-p never land on a dir
    app.handle(key('g'));
    app.handle(key('g'));
    assert!(matches!(
        tree_kinds(&app).get(tree_cursor(&app)).map(String::as_str),
        Some("dir")
    ));
    // from the dir row, c-n jumps to the first file below it
    app.handle(ctrl_key('n'));
    assert_eq!(selected_path(&app), "src/lib.rs");
    let mut visited = vec![selected_path(&app)];
    // walk forward to the end, recording every stop is a file
    for _ in 0..2 {
        app.handle(ctrl_key('n'));
        assert!(matches!(
            tree_kinds(&app).get(tree_cursor(&app)).map(String::as_str),
            Some(kind) if kind.starts_with("file:")
        ));
        visited.push(selected_path(&app));
    }
    assert_eq!(visited, vec!["src/lib.rs", "ci.yml", "todo.md"]);
    // and back the other way, still only files
    app.handle(ctrl_key('p'));
    assert_eq!(selected_path(&app), "ci.yml");
}

#[test]
fn brackets_step_the_sidebar_folder_by_folder() {
    let fixture = nested_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
    // the tree opens with the cursor on the selected file, inside a folder
    assert_eq!(tree_cursor_row(&app), "file readme.md");
    app.handle(key('['));
    assert_eq!(
        tree_cursor_row(&app),
        "dir docs",
        "back to the folder holding it"
    );
    app.handle(key(']'));
    assert_eq!(tree_cursor_row(&app), "dir src");
    app.handle(key(']'));
    assert_eq!(
        tree_cursor_row(&app),
        "dir src/deep",
        "a nested folder counts"
    );
    app.handle(key('['));
    assert_eq!(tree_cursor_row(&app), "dir src", "and back up");
}

#[test]
fn brackets_step_the_sidebar_section_by_section() {
    let fixture = kinds_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    app.handle(key(']'));
    assert_eq!(tree_cursor_row(&app), "section Generated");
    app.handle(key('['));
    assert_eq!(tree_cursor_row(&app), "section Source");
}

/// The last group has nothing after it, so the cursor stays put.
#[test]
fn stepping_past_the_last_group_stands_still() {
    let fixture = nested_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
    app.handle(key('G'));
    let last = tree_cursor_row(&app);
    app.handle(key(']'));
    assert_eq!(tree_cursor_row(&app), last);
}

/// The diff pane keeps the brackets for hunks: the motion moves the pane
/// holding the keyboard.
#[test]
fn brackets_in_the_diff_pane_still_step_hunks() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    app.handle(key('\n'));
    assert_eq!(focus(&app), Pane::Diff);
    let before = app.diff.as_ref().expect("diff").cursor;
    app.handle(key(']'));
    let diff = app.diff.as_ref().expect("diff");
    assert!(
        matches!(diff.rows.get(diff.cursor), Some(DiffRow::Hunk { .. })) || diff.cursor == before
    );
}

#[test]
fn t_cycles_the_sidebar_through_the_tree_the_review_buckets_and_the_kinds() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    assert!(tree_kinds(&app).contains(&"dir".to_owned()));
    app.handle(key('t'));
    assert_eq!(tree_kinds(&app)[0], "section:To review:3");
    app.handle(key('t'));
    assert_eq!(tree_kinds(&app)[0], "section:Source:1");
    app.handle(key('t'));
    assert!(
        tree_kinds(&app).contains(&"dir".to_owned()),
        "wraps back to the tree"
    );
}

#[test]
fn the_kinds_layout_groups_by_what_a_file_is_and_hides_the_empty_buckets() {
    let fixture = standard_fixture();
    let app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    assert_eq!(
        tree_kinds(&app),
        vec![
            "section:Source:1",
            "file:src/lib.rs",
            "section:Docs:1",
            "file:todo.md",
            "section:Config:1",
            "file:ci.yml",
        ],
        "one header per kind the diff has, none for the kinds it does not"
    );
}

#[test]
fn the_repos_declared_kinds_land_from_the_worker_and_regroup_the_sidebar() {
    let fixture = standard_fixture();
    let app = &mut diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    assert!(tree_kinds(app).contains(&"section:Source:1".to_owned()));
    let request = app.pending_declared.take().expect("a queued lookup");
    assert!(request.paths.contains(&"src/lib.rs".to_owned()));

    let kinds = HashMap::from([("src/lib.rs".to_owned(), Kind::Generated)]);
    app.handle(AppEvent::DeclaredKinds {
        kinds,
        token: request.token,
    });

    assert!(
        tree_kinds(app).contains(&"section:Generated:1".to_owned()),
        "{:?}",
        tree_kinds(app)
    );
}

#[test]
fn an_answer_for_a_replaced_file_list_is_dropped() {
    let fixture = standard_fixture();
    let app = &mut diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    let stale = app.pending_declared.take().expect("a queued lookup").token;
    // the view moves on, so the answer in flight is about the old list
    app.queue_declared();

    app.handle(AppEvent::DeclaredKinds {
        kinds: HashMap::from([("src/lib.rs".to_owned(), Kind::Generated)]),
        token: stale,
    });

    assert!(tree_kinds(app).contains(&"section:Source:1".to_owned()));
}

#[test]
fn a_reader_glob_moves_a_file_between_kind_buckets() {
    let fixture = standard_fixture();
    let mut loaded = LoadedConfig::default();
    loaded.config.ui.diff_file_layout = crate::config::FileLayout::Kinds;
    loaded.config.classify.tests = vec!["src/**".to_owned()];
    let mut app = App::new(fixture.review(), loaded);
    app.open_working_tree_diff(None);
    assert!(
        tree_kinds(&app).contains(&"section:Tests:1".to_owned()),
        "{:?}",
        tree_kinds(&app)
    );
}

#[test]
fn cycling_the_sidebar_layout_drops_a_committed_search() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    app.handle(key('/'));
    app.handle(key('l'));
    app.handle(key('\n'));
    assert!(app.search.is_some(), "search committed on the tree rows");
    app.handle(key('t'));
    assert!(
        app.search.is_none(),
        "layout swap invalidates the search row indices"
    );
}
