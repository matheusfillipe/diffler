//! Tests for how a refresh keeps the cursor and selection in place.

use super::*;

#[test]
fn noop_refresh_preserves_the_visual_selection() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
    app.handle(key('V'));
    assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
    // nothing changed on disk: a watcher echo or poll tick refresh
    // must not kill the selection
    app.handle(AppEvent::RepoChanged);
    app.settle_refresh();
    assert!(
        app.diff.as_ref().unwrap().visual_anchor.is_some(),
        "no-op refresh keeps the selection"
    );
}

/// The `new_no` the cursor's own row carries, read fresh each time since
/// a rebuild can move it to a different row index.
fn cursor_line_no(app: &App) -> u32 {
    let diff = app.diff.as_ref().unwrap();
    let model = diff.model(&app.review);
    let DiffRow::Line { hunk, line, .. } = diff.rows()[diff.cursor] else {
        panic!("cursor sits on a line")
    };
    model.files[0].hunks[hunk].lines[line].new_no.unwrap()
}

#[test]
fn the_cursor_stays_on_the_same_line_after_a_comment_lands_above_it() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    app.author = "reviewer".to_owned();
    enter_diff_pane(&mut app);
    cursor_to_line(&mut app, |r| {
        matches!(
            r,
            DiffRow::Line {
                hunk: 1,
                line: 2,
                ..
            }
        )
    });
    let before_row = app.diff.as_ref().unwrap().cursor;
    let before_line = cursor_line_no(&app);

    // an unanchored comment lands at the very top of the file, above
    // every hunk the cursor could be on
    app.review.session.add_comment(
        Anchor {
            file: "data.txt".to_owned(),
            line: None,
            line_end: None,
            on_old_side: false,
            line_text: None,
        },
        "reviewer",
        "look here first",
    );
    let diff = app.diff.as_mut().unwrap();
    diff.mark_rows_dirty();
    diff.ensure_rows(&app.review);

    assert_ne!(
        app.diff.as_ref().unwrap().cursor,
        before_row,
        "the new comment pushed the row down"
    );
    assert_eq!(
        cursor_line_no(&app),
        before_line,
        "and the cursor followed it"
    );
}

#[test]
fn the_cursor_stays_on_the_same_line_after_a_refresh_rebuilds_every_hunk() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    cursor_to_line(&mut app, |r| {
        matches!(
            r,
            DiffRow::Line {
                hunk: 1,
                line: 2,
                ..
            }
        )
    });
    let before = cursor_line_no(&app);

    // a third, unrelated edit in the middle of the file inserts a new
    // hunk before the one the cursor sits in, shifting every later
    // hunk's index
    let lines: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
    let mut edited = lines;
    edited[0] = "line one".to_owned();
    edited[9] = "line ten edited".to_owned();
    edited[19] = "line twenty".to_owned();
    fixture.write("data.txt", &(edited.join("\n") + "\n"));
    app.handle(AppEvent::RepoChanged);
    app.settle_refresh();

    let diff = app.diff.as_ref().unwrap();
    assert_eq!(
        diff.rows()
            .iter()
            .filter(|r| matches!(r, DiffRow::Hunk { .. }))
            .count(),
        3,
        "the new edit split off a third hunk, shifting the one the cursor was in"
    );
    assert_eq!(
        cursor_line_no(&app),
        before,
        "the cursor followed the same line even though its hunk index moved"
    );
}

#[test]
fn a_refresh_elsewhere_in_the_repo_keeps_the_visual_selection() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
    app.handle(key('V'));
    // a new, unrelated file changes the model's fingerprint, but the
    // selected file's own lines never moved
    fixture.write("zzz.md", "new\n");
    app.handle(AppEvent::RepoChanged);
    app.settle_refresh();
    assert!(
        app.diff.as_ref().unwrap().visual_anchor.is_some(),
        "the selected rows are still there, so the selection survives"
    );
}

#[test]
fn a_refresh_while_composing_leaves_the_cursor_on_the_caret_line() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
    app.handle(key('c'));
    type_text(&mut app, "still typing");
    assert!(app.composer_open(), "composer stayed open");
    assert_eq!(
        app.diff.as_ref().unwrap().cursor,
        caret_row(&app),
        "typing seats the cursor on the caret"
    );

    // an unrelated file change forces a real refresh, rebuilding the rows
    fixture.write("zzz.md", "new\n");
    app.handle(AppEvent::RepoChanged);
    app.settle_refresh();

    assert!(app.composer_open(), "the draft survives the refresh");
    assert_eq!(
        app.diff.as_ref().unwrap().cursor,
        caret_row(&app),
        "the refresh leaves the cursor on the caret's line, not the card's header"
    );
}

#[test]
fn a_selection_whose_rows_are_gone_ends_rather_than_covering_something_else() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { hunk: 0, .. }));
    app.handle(key('V'));
    app.handle(key('j'));
    assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
    // revert the first hunk's edit: those rows leave the diff entirely
    let lines: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
    let mut edited = lines;
    edited[19] = "line twenty".to_owned();
    fixture.write("data.txt", &(edited.join("\n") + "\n"));
    app.handle(AppEvent::RepoChanged);
    app.settle_refresh();
    assert!(
        app.diff.as_ref().unwrap().visual_anchor.is_none(),
        "the selected rows are gone, so the selection ends rather than \
         latching onto whatever now sits at the old index"
    );
}

#[test]
fn a_banded_span_still_covers_the_same_lines_after_a_card_lands_inside_it() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    app.author = "reviewer".to_owned();
    let id = app
        .review
        .session
        .add_comment(
            Anchor {
                file: "data.txt".to_owned(),
                line: Some(2),
                line_end: Some(4),
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "range comment",
        )
        .id
        .clone();
    app.diff.as_mut().unwrap().mark_rows_dirty();
    assert!(app.focus_comment(&id));
    let line_no = |app: &App, row: usize| -> u32 {
        let diff = app.diff.as_ref().unwrap();
        let model = diff.model(&app.review);
        let DiffRow::Line { hunk, line, .. } = diff.rows()[row] else {
            panic!("band row is a line")
        };
        model.files[0].hunks[hunk].lines[line].new_no.unwrap()
    };
    let (start, end) = app
        .diff
        .as_ref()
        .unwrap()
        .referenced
        .expect("a banded span");
    assert_eq!(line_no(&app, start), 2);
    assert_eq!(line_no(&app, end), 4);

    // a second comment anchored on line 3 lands its card between the
    // span's own endpoints
    app.review.session.add_comment(
        Anchor {
            file: "data.txt".to_owned(),
            line: Some(3),
            line_end: None,
            on_old_side: false,
            line_text: None,
        },
        "reviewer",
        "in between",
    );
    let diff = app.diff.as_mut().unwrap();
    diff.mark_rows_dirty();
    diff.ensure_rows(&app.review);

    let (start, end) = app
        .diff
        .as_ref()
        .unwrap()
        .referenced
        .expect("the band survives a card landing inside it");
    assert_eq!(line_no(&app, start), 2, "band still opens on line 2");
    assert_eq!(line_no(&app, end), 4, "band still closes on line 4");
    assert!(
        end - start > 2,
        "the new card's rows now sit inside the band"
    );
}

#[test]
fn refresh_keeps_the_selected_file_by_path_when_files_shift() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    let path = selected_path(&app);
    // a new file ahead of src/ shifts every file index
    fixture.write("aaa.rs", "fn nope() {}\n");
    app.handle(ctrl_key('r'));
    assert_eq!(
        selected_path(&app),
        path,
        "selection follows its file across the refresh"
    );
}
