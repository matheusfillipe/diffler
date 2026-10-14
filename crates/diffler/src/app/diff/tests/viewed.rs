//! Tests for marking files and groups viewed.

use super::*;

#[test]
fn viewing_a_directory_marks_the_whole_subtree_and_nothing_outside_it() {
    let fixture = nested_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
    cursor_to_dir(&mut app, "src");
    app.handle(key('m'));
    assert_eq!(
        viewed_paths(&app),
        ["src/deep/inner.rs", "src/lib.rs", "src/main.rs"],
        "every file under src, including the nested one"
    );
}

#[test]
fn viewing_a_nested_directory_leaves_its_parent_alone() {
    let fixture = nested_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
    cursor_to_dir(&mut app, "src/deep");
    app.handle(key('m'));
    assert_eq!(viewed_paths(&app), ["src/deep/inner.rs"]);
}

#[test]
fn viewing_an_already_viewed_directory_puts_it_back() {
    let fixture = standard_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
    cursor_to_dir(&mut app, "src");
    app.handle(key('m'));
    assert!(!viewed_paths(&app).is_empty());
    cursor_to_dir(&mut app, "src");
    app.handle(key('m'));
    assert!(
        viewed_paths(&app).is_empty(),
        "a second press unmarks the subtree"
    );
}

/// A kind header stands for every file in its bucket, and Generated opens
/// folded, so the mark cannot come from the rows on screen.
#[test]
fn viewing_a_kind_section_marks_every_file_it_holds() {
    let fixture = kinds_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    let rows = super::sidebar_rows(app.diff.as_ref().expect("diff"), &app.review);
    assert!(
        rows.iter().any(|row| matches!(&row.node,
            crate::tree::TreeNode::Section { bucket, folded, .. }
                if bucket.label() == "Generated" && *folded)),
        "Generated opens folded, so the mark cannot read the rows: {rows:?}"
    );
    cursor_to_section(&mut app, "Generated");
    app.handle(key('m'));
    assert_eq!(
        viewed_paths(&app),
        ["package-lock.json", "tests/snapshots/render.snap"],
        "the whole bucket, folded though it is"
    );
}

#[test]
fn viewing_a_kind_section_again_puts_it_back() {
    let fixture = kinds_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    cursor_to_section(&mut app, "Generated");
    app.handle(key('m'));
    assert!(!viewed_paths(&app).is_empty());
    cursor_to_section(&mut app, "Generated");
    app.handle(key('m'));
    assert!(viewed_paths(&app).is_empty(), "a second press unmarks it");
}

#[test]
fn viewing_the_to_review_section_clears_the_whole_review() {
    let fixture = kinds_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
    cursor_to_section(&mut app, "To review");
    app.handle(key('m'));
    assert_eq!(
        viewed_paths(&app),
        [
            "package-lock.json",
            "src/lib.rs",
            "tests/snapshots/render.snap"
        ]
    );
}

/// The bucket a header stands for is read from the grouping, so a kind the
/// diff has nothing for cannot be reached and its neighbours are untouched.
#[test]
fn viewing_one_kind_leaves_the_others_alone() {
    let fixture = kinds_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    cursor_to_section(&mut app, "Source");
    app.handle(key('m'));
    assert_eq!(viewed_paths(&app), ["src/lib.rs"]);
}

/// Marking the last file in a group sorts it to the top of that group,
/// and the cursor stays on its row, now the file that was above.
#[test]
fn marking_the_bottom_file_keeps_the_reader_at_the_bottom() {
    for layout in [
        crate::config::FileLayout::Tree,
        crate::config::FileLayout::Kinds,
    ] {
        let fixture = flat_fixture();
        let mut app = diff_app_with_layout(&fixture, layout);
        app.handle(key('G'));
        assert_eq!(selected_path(&app), "d.rs", "{layout:?}");
        let row = app.diff.as_ref().expect("diff").tree_cursor;

        app.handle(key('m'));
        assert_eq!(
            selected_path(&app),
            "c.rs",
            "{layout:?}: the next one to read, not the file that moved"
        );
        assert_eq!(
            app.diff.as_ref().expect("diff").tree_cursor,
            row,
            "{layout:?}: and the cursor holds its row"
        );
    }
}

/// Marking up the list from the bottom keeps walking the unviewed files.
#[test]
fn marking_from_the_bottom_walks_up_through_what_is_left() {
    let fixture = flat_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    app.handle(key('G'));
    let mut walked = vec![selected_path(&app)];
    for _ in 0..3 {
        app.handle(key('m'));
        walked.push(selected_path(&app));
    }
    assert_eq!(walked, ["d.rs", "c.rs", "b.rs", "a.rs"]);
}

/// With a file below, the walk still goes down: the hold only applies when
/// there is nothing under the one just marked.
#[test]
fn marking_with_a_file_below_still_steps_down() {
    let fixture = flat_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    assert_eq!(selected_path(&app), "a.rs");
    app.handle(key('m'));
    assert_eq!(selected_path(&app), "b.rs");
}

#[test]
fn viewing_a_file_row_still_marks_only_that_file() {
    let fixture = standard_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Tree);
    select_file(&mut app, "src/lib.rs");
    app.handle(key('m'));
    assert_eq!(viewed_paths(&app), ["src/lib.rs"]);
}

/// The walk follows the sidebar order: the row under the one just marked
/// is where the reader was heading.
#[test]
fn marking_viewed_steps_to_the_file_listed_under_it() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    assert_eq!(
        tree_kinds(&app),
        vec!["dir", "file:lib.rs", "file:ci.yml", "file:todo.md"],
        "directories first, so the sidebar order is not the model order"
    );
    assert_eq!(selected_path(&app), "ci.yml");

    app.handle(key('m'));
    assert!(app.is_path_viewed("ci.yml"));
    assert_eq!(
        selected_path(&app),
        "todo.md",
        "the row under ci.yml, not the next file in the diff"
    );

    app.handle(key('m'));
    assert!(app.is_path_viewed("todo.md"));
    assert_eq!(
        selected_path(&app),
        "todo.md",
        "nothing below: the selection stays"
    );
}

/// What is done piles at the top of a directory, so the run still to read
/// is the bottom of the group and never moves out from under the cursor.
#[test]
fn a_viewed_file_sorts_to_the_top_of_its_directory() {
    let fixture = grouped_fixture();
    let mut app = diff_app(&fixture);
    assert_eq!(
        tree_kinds(&app),
        vec!["dir", "file:a.rs", "file:b.rs", "file:c.rs"]
    );

    select_file(&mut app, "src/b.rs");
    app.handle(key('m'));
    assert_eq!(
        tree_kinds(&app),
        vec!["dir", "file:b.rs", "file:a.rs", "file:c.rs"],
        "b.rs sorts above what is left to read"
    );
    assert_eq!(
        selected_path(&app),
        "src/c.rs",
        "the row that was under b.rs"
    );

    app.handle(key('m'));
    assert_eq!(
        tree_kinds(&app),
        vec!["dir", "file:b.rs", "file:c.rs", "file:a.rs"],
        "the viewed run keeps the order the diff gave it"
    );
}

/// The walk stops at what is on screen and reports the folded files it
/// skipped; `u` reaches them.
#[test]
fn the_walk_stops_at_a_folded_group_and_says_what_is_left() {
    let fixture = Fixture::new();
    fixture.write("ci.yml", "on: push\n");
    fixture.write("Cargo.lock", "# generated\n");
    fixture.commit_all("base");
    fixture.write("ci.yml", "on: [push]\n");
    fixture.write("Cargo.lock", "# generated, again\n");
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    // Generated starts folded, so Cargo.lock has no row of its own
    assert_eq!(
        tree_kinds(&app),
        vec!["section:Config:1", "file:ci.yml", "section:Generated:1"]
    );

    select_file(&mut app, "ci.yml");
    app.handle(key('m'));
    assert_eq!(
        selected_path(&app),
        "ci.yml",
        "the folded section is not walked into"
    );
    assert_eq!(
        app.message.as_ref().map(|m| m.text.as_str()),
        Some("end of the list, 1 still unviewed")
    );

    // u reaches it, folded or not
    app.handle(key('u'));
    assert_eq!(selected_path(&app), "Cargo.lock");
}

#[test]
fn a_viewed_file_sorts_to_the_top_of_its_kind_section() {
    let fixture = grouped_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Kinds);
    assert_eq!(
        tree_kinds(&app),
        vec![
            "section:Source:3",
            "file:src/a.rs",
            "file:src/b.rs",
            "file:src/c.rs"
        ]
    );

    select_file(&mut app, "src/b.rs");
    app.handle(key('m'));
    assert_eq!(
        tree_kinds(&app),
        vec![
            "section:Source:3",
            "file:src/b.rs",
            "file:src/a.rs",
            "file:src/c.rs"
        ]
    );
    assert_eq!(selected_path(&app), "src/c.rs");
}

#[test]
fn capital_u_clears_all_viewed_marks() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    let paths: Vec<String> = app
        .review
        .model()
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect();
    for path in &paths {
        select_file(&mut app, path);
        app.handle(key('m'));
    }
    assert!(
        paths.iter().all(|p| app.is_path_viewed(p)),
        "all marked first"
    );

    app.handle(key('U'));
    assert!(
        paths.iter().all(|p| !app.is_path_viewed(p)),
        "U cleared every viewed mark"
    );
}

#[test]
fn unmarking_viewed_does_not_move_the_selection() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    let first = selected_path(&app);
    assert_eq!(first, "ci.yml");
    app.handle(key('m'));
    select_file(&mut app, &first);
    app.handle(key('m'));
    assert!(!app.is_path_viewed(&first));
    assert_eq!(
        selected_path(&app),
        first,
        "unmarking keeps the selection in place"
    );
}

#[test]
fn viewed_can_be_marked_from_the_diff_pane() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    let first = selected_path(&app);
    enter_diff_pane(&mut app);
    app.handle(key('m'));
    assert!(app.is_path_viewed(&first));
    // marking advanced the selection past the viewed file
    assert_ne!(selected_path(&app), first);
}

#[test]
fn review_layout_buckets_files_with_the_viewed_pile_folded() {
    let fixture = standard_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
    assert_eq!(
        tree_kinds(&app),
        [
            "section:To review:3",
            "file:ci.yml",
            "file:src/lib.rs",
            "file:todo.md",
            "section:Viewed:0",
        ]
    );
    // v moves the file into the folded viewed bucket and advances
    app.handle(key('m'));
    assert_eq!(
        tree_kinds(&app),
        [
            "section:To review:2",
            "file:src/lib.rs",
            "file:todo.md",
            "section:Viewed:1",
        ]
    );
    assert_eq!(selected_path(&app), "src/lib.rs");
}

#[test]
fn toggling_the_viewed_bucket_reveals_and_hides_its_files() {
    let fixture = standard_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
    app.handle(key('m'));
    // the folded viewed header is the last row; za on it unfolds
    app.handle(key('G'));
    app.handle(key('z'));
    app.handle(key('a'));
    assert_eq!(
        tree_kinds(&app),
        [
            "section:To review:2",
            "file:src/lib.rs",
            "file:todo.md",
            "section:Viewed:1",
            "file:ci.yml",
        ]
    );
    // <cr> on the header folds it back
    app.handle(key('\n'));
    assert_eq!(
        tree_kinds(&app).last().map(String::as_str),
        Some("section:Viewed:1")
    );
}

#[test]
fn editing_a_viewed_file_returns_it_to_the_to_review_bucket() {
    let fixture = standard_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
    app.handle(key('m'));
    assert_eq!(tree_kinds(&app)[0], "section:To review:2");
    // the file changes on disk: its hash no longer matches the mark
    fixture.write("ci.yml", "on: push\njobs: {}\n");
    app.handle(AppEvent::RepoChanged);
    app.settle_refresh();
    assert_eq!(
        tree_kinds(&app)[0],
        "section:To review:3",
        "stale viewed mark drops the file back into to-review"
    );
}

#[test]
fn unmarking_in_the_review_layout_reseats_the_cursor_on_the_moved_row() {
    let fixture = standard_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
    // view everything, then unfold the viewed bucket and step onto a file
    app.handle(key('m'));
    app.handle(key('m'));
    app.handle(key('m'));
    app.handle(key('G'));
    app.handle(key('z'));
    app.handle(key('a'));
    app.handle(key('j'));
    assert_eq!(selected_path(&app), "ci.yml");
    // unmark: ci.yml jumps up into to-review; the cursor must follow it
    app.handle(key('m'));
    assert_eq!(
        tree_kinds(&app),
        [
            "section:To review:1",
            "file:ci.yml",
            "section:Viewed:2",
            "file:src/lib.rs",
            "file:todo.md",
        ]
    );
    assert_eq!(tree_cursor(&app), 1, "cursor sits on the moved file row");
}

#[test]
fn marking_the_last_unviewed_file_keeps_the_cursor_in_range() {
    let fixture = standard_fixture();
    let mut app = diff_app_with_layout(&fixture, crate::config::FileLayout::Review);
    // walk the files, marking each: the last one empties the to-review
    // bucket under the cursor
    for _ in 0..app
        .diff
        .as_ref()
        .expect("diff")
        .model(&app.review)
        .files
        .len()
    {
        app.handle(key('m'));
    }
    let rows = tree_row_count(&app);
    assert_eq!(rows, 2, "both buckets, all files folded away");
    assert!(
        tree_cursor(&app) < rows,
        "cursor clamps into the shrunken row list"
    );
}

#[test]
fn u_jumps_to_the_next_unviewed_file_wrapping_past_viewed_ones() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    app.handle(key('m')); // ci.yml viewed, selection lands on todo.md
    select_file(&mut app, "todo.md");
    app.handle(key('u'));
    assert_eq!(
        selected_path(&app),
        "src/lib.rs",
        "wraps past the viewed ci.yml"
    );
}

#[test]
fn u_with_everything_viewed_says_so() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    for path in ["ci.yml", "src/lib.rs", "todo.md"] {
        select_file(&mut app, path);
        app.handle(key('m'));
    }
    let before = selected_path(&app);
    app.handle(key('u'));
    assert_eq!(selected_path(&app), before);
    let message = app.message.expect("message");
    assert!(message.text.contains("every file is viewed"));
}
