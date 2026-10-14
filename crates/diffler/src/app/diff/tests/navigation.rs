//! Tests for opening a diff and moving the cursor and focus through it.

use super::*;

/// An empty review has no rows to show, so the opener declines and the
/// reader stays put.
#[test]
fn opening_a_review_with_no_files_says_so_and_opens_nothing() {
    let fixture = Fixture::new();
    fixture.write("a.rs", "fn a() {}\n");
    fixture.commit_all("base");
    let mut app = App::new(fixture.review(), LoadedConfig::default());

    app.handle(key('D'));
    assert!(app.diff.is_none(), "no review opened over a clean tree");
    assert_eq!(app.screen(), Screen::Status);
    assert_eq!(
        app.message.as_ref().map(|m| m.text.as_str()),
        Some("nothing to review: working tree clean")
    );

    // an empty commit reaches the same guard, named by its own source
    fixture.commit_all("nothing");
    let head = app.review.vcs.resolve("HEAD").expect("head oid");
    app.open_commit_diff(&head);
    assert!(app.diff.is_none(), "no review opened over an empty commit");
    let message = app.message.as_ref().expect("message").text.clone();
    assert!(
        message.starts_with("nothing to review in commit"),
        "the source names itself: {message}"
    );
}

#[test]
fn rows_flatten_the_selected_files_hunks_and_lines_in_order() {
    let fixture = two_hunk_fixture();
    let app = diff_app(&fixture);
    let rows = rows(&app);
    // no file header row: the selected file is implicit
    assert!(matches!(rows.first(), Some(DiffRow::Hunk { hunk: 0, .. })));
    let hunks: Vec<usize> = rows
        .iter()
        .filter_map(|r| match r {
            DiffRow::Hunk { hunk, .. } => Some(*hunk),
            _ => None,
        })
        .collect();
    assert_eq!(hunks, vec![0, 1], "both hunks flattened in order");
    assert!(
        rows.iter()
            .any(|r| matches!(r, DiffRow::Line { hunk: 1, .. })),
        "second hunk has line rows"
    );
}

#[test]
fn scoped_open_selects_the_file_and_focuses_the_diff() {
    let fixture = standard_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.open_working_tree_file("src/lib.rs");
    assert_eq!(focus(&app), Pane::Diff);
    assert_eq!(selected_path(&app), "src/lib.rs");
    assert_eq!(app.diff.as_ref().unwrap().cursor, 0, "starts at the top");
}

#[test]
fn e_on_a_diff_line_opens_the_editor_at_that_line() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    // pin the editor through config so the test ignores $EDITOR
    app.config.editor.command = Some("vim".to_owned());
    select_file(&mut app, "src/lib.rs");
    let position = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = position;
    app.handle(key('e'));
    let request = app.pending_editor.clone().expect("editor request");
    assert_eq!(
        request.purpose,
        crate::editor::EditorPurpose::OpenFile {
            path: "src/lib.rs".to_owned(),
        }
    );
    // the "42" line is line 2 on the new side
    let absolute = fixture.root.join("src/lib.rs");
    assert_eq!(
        request.cmd,
        vec![
            "vim".to_owned(),
            "+2".to_owned(),
            absolute.to_string_lossy().into_owned(),
        ]
    );
}

#[test]
fn e_from_the_list_opens_the_selected_file_without_a_line() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    app.config.editor.command = Some("vim".to_owned());
    assert_eq!(focus(&app), Pane::List);
    app.handle(key('e'));
    let request = app.pending_editor.clone().expect("editor request");
    assert!(
        request.cmd.iter().all(|arg| !arg.starts_with('+')),
        "no line jump from the list: {:?}",
        request.cmd
    );
}

#[test]
fn half_page_motions_move_by_the_viewport() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    app.diff.as_mut().unwrap().viewport = 10;
    app.handle(ctrl_key('d'));
    assert_eq!(app.diff.as_ref().unwrap().cursor, 5);
    app.handle(ctrl_key('u'));
    assert_eq!(app.diff.as_ref().unwrap().cursor, 0);
}

/// A paging key belongs to the pane holding the keyboard, so on the
/// sidebar it walks the file list and leaves the diff where it was.
#[test]
fn list_focus_half_page_walks_the_file_list() {
    let fixture = Fixture::new();
    for index in 0..30 {
        fixture.write(&format!("f{index:02}.txt"), "one\n");
    }
    fixture.commit_all("base");
    for index in 0..30 {
        fixture.write(&format!("f{index:02}.txt"), "one\ntwo\n");
    }
    let mut app = diff_app(&fixture);
    assert_eq!(focus(&app), Pane::List);
    app.diff.as_mut().unwrap().sidebar.height = 10;
    app.diff.as_mut().unwrap().viewport = 10;

    app.handle(ctrl_key('d'));
    assert_eq!(tree_cursor(&app), 5, "half the sidebar's rows");
    assert_eq!(
        app.diff.as_ref().unwrap().cursor,
        0,
        "the diff pane stays where it was"
    );
    assert_eq!(focus(&app), Pane::List);

    app.handle(ctrl_key('u'));
    assert_eq!(tree_cursor(&app), 0);
    // and the whole-page keys cover the whole pane
    app.handle(ctrl_key('f'));
    assert_eq!(tree_cursor(&app), 9);
}

#[test]
fn comments_focus_half_page_walks_the_cards() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    for line in 1..=6 {
        app.review.session.add_comment(
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(line),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "look here",
        );
    }
    app.handle(key('C'));
    assert_eq!(focus(&app), Pane::Comments);
    // one render measures the pane and the rows each card takes
    crate::test_support::render(&mut app);

    let cursor = |app: &App| app.diff.as_ref().expect("diff").comments_cursor();
    assert_eq!(cursor(&app), 0);
    app.handle(ctrl_key('d'));
    let tall = cursor(&app);
    assert!(tall > 1, "a page covers several cards, moved {tall}");
    app.handle(ctrl_key('u'));
    assert_eq!(cursor(&app), 0, "and comes back by the same step");

    // the step is what the pane can show, so a short pane covers fewer
    app.diff.as_mut().expect("diff").comments_rect.height = 6;
    app.handle(ctrl_key('d'));
    let short = cursor(&app);
    assert!(
        short < tall,
        "a 6-row pane pages {short} cards, a full-height one {tall}"
    );
}

#[test]
fn hunk_jumps_move_between_headers() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    // the cursor starts on the first hunk header (no file row precedes it)
    let first = app.diff.as_ref().unwrap().cursor;
    assert!(matches!(rows(&app)[first], DiffRow::Hunk { hunk: 0, .. }));
    app.handle(key(']'));
    assert!(matches!(
        rows(&app)[app.diff.as_ref().unwrap().cursor],
        DiffRow::Hunk { hunk: 1, .. }
    ));
    app.handle(key('['));
    assert_eq!(app.diff.as_ref().unwrap().cursor, first);
}

#[test]
fn paren_jumps_land_on_definition_starts() {
    use crate::test_support::standard_fixture;
    let fixture = standard_fixture();
    fixture.write(
        "src/lib.rs",
        "pub fn answer() -> u32 {\n    42\n}\n\npub fn other() -> u32 {\n    7\n}\n",
    );
    let mut app = crate::app::App::new(fixture.review(), crate::config::LoadedConfig::default());
    app.open_working_tree_diff(Some("src/lib.rs"));
    let (path, content) = ("src/lib.rs".to_owned(), fixture_content(&app));
    let index = app.highlighter.scope_index(&path, &content);
    assert!(!index.is_empty(), "rust definitions indexed");
    let hash = current_hash(&app);
    if let Some(diff) = app.diff.as_mut() {
        diff.scopes
            .insert(path, crate::app::diff::FileScope { hash, index });
        diff.ensure_rows(&app.review);
        diff.focus = Pane::Diff;
    }
    app.handle(crate::test_support::key(')'));
    let diff = app.diff.as_ref().expect("diff");
    let on_def = matches!(
        diff.rows().get(diff.cursor),
        Some(DiffRow::Line { file, hunk, line })
            if app.review.model().files.get(*file)
                .and_then(|f| f.hunks.get(*hunk))
                .and_then(|h| h.lines.get(*line))
                .is_some_and(|l| l.text.contains("fn "))
    );
    assert!(on_def, "cursor row {} is a definition line", diff.cursor);
}
