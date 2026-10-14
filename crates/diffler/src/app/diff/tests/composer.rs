//! Tests for the in-place comment composer.

use super::*;

/// Cursor on the added line, `c` open, a note typed: the setup every
/// composer editor test starts from.
fn composer_with_typed_note(app: &mut App) {
    select_file(app, "src/lib.rs");
    let position = added_line_position(app);
    app.diff.as_mut().unwrap().cursor = position;
    app.handle(key('c'));
    type_text(app, "why 42?");
}

#[test]
fn ctrl_g_sends_the_composer_buffer_to_the_editor_and_reads_it_back() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    composer_with_typed_note(&mut app);

    app.handle(ctrl_key('g'));
    let request = app.pending_editor.take().expect("editor queued");
    let (path, target) = match &request.purpose {
        crate::editor::EditorPurpose::TextBox { path, target } => (path.clone(), *target),
        other => panic!("expected a text box purpose, got {other:?}"),
    };
    assert_eq!(target, crate::editor::TextBoxTarget::Composer);
    assert!(
        path.starts_with(std::env::temp_dir()),
        "scratch, not the gitdir: {}",
        path.display()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "why 42?");
    assert!(
        app.composer_open(),
        "the composer stays put while suspended"
    );

    std::fs::write(&path, "edited externally").unwrap();
    app.editor_finished(request.purpose, Ok(true));

    assert!(!path.exists(), "the scratch file is removed");
    let composer = app
        .diff
        .as_ref()
        .unwrap()
        .composer
        .as_ref()
        .expect("composer stays open");
    assert_eq!(composer.buffer, "edited externally");
}

#[test]
fn a_cancelled_composer_edit_keeps_the_buffer() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    composer_with_typed_note(&mut app);

    app.handle(ctrl_key('g'));
    let request = app.pending_editor.take().expect("editor queued");
    let path = match &request.purpose {
        crate::editor::EditorPurpose::TextBox { path, .. } => path.clone(),
        other => panic!("expected a text box purpose, got {other:?}"),
    };
    // the editor wrote something before exiting non-zero (e.g. vim's :cq)
    std::fs::write(&path, "an edit the editor never saved").unwrap();

    app.editor_finished(request.purpose, Ok(false));

    assert!(!path.exists(), "the scratch file is removed regardless");
    let composer = app
        .diff
        .as_ref()
        .unwrap()
        .composer
        .as_ref()
        .expect("composer stays open");
    assert_eq!(
        composer.buffer, "why 42?",
        "the cancelled edit changed nothing"
    );
}

#[test]
fn a_failed_composer_editor_keeps_the_buffer_and_says_so() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    composer_with_typed_note(&mut app);

    app.handle(ctrl_key('g'));
    let request = app.pending_editor.take().expect("editor queued");
    let path = match &request.purpose {
        crate::editor::EditorPurpose::TextBox { path, .. } => path.clone(),
        other => panic!("expected a text box purpose, got {other:?}"),
    };

    app.editor_finished(request.purpose, Err("boom".to_owned()));

    assert!(!path.exists(), "the scratch file is removed regardless");
    let composer = app
        .diff
        .as_ref()
        .unwrap()
        .composer
        .as_ref()
        .expect("composer stays open");
    assert_eq!(composer.buffer, "why 42?");
    let message = app.message.clone().expect("message");
    assert_eq!(message.severity, crate::app::Severity::Error);
    assert!(message.text.contains("editor failed"));
    assert!(message.text.contains("boom"));
}

/// Row indices of the composer's rows, in order.
fn composer_rows(app: &App) -> Vec<usize> {
    app.diff
        .as_ref()
        .expect("diff view")
        .rows()
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, DiffRow::Composer { .. }))
        .map(|(index, _)| index)
        .collect()
}

#[test]
fn the_composer_takes_the_rows_under_the_line_it_comments_on() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    let line = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = line;
    app.handle(key('c'));
    let rows = composer_rows(&app);
    assert_eq!(
        rows.first().copied(),
        Some(line + 1),
        "the card opens directly under its line"
    );
    assert!(rows.len() >= 3, "header, a body row, and the hint footer");
    assert!(
        rows.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "the card is one contiguous block: {rows:?}"
    );
}

#[test]
fn the_composer_grows_a_row_when_the_text_wraps() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.diff.as_mut().unwrap().set_wrap_width(60);
    app.handle(key('c'));
    let before = composer_rows(&app).len();
    type_text(
        &mut app,
        &"x".repeat(super::super::super::composer::card_budget(60) + 1),
    );
    assert_eq!(
        composer_rows(&app).len(),
        before + 1,
        "one more row once the text passes the wrap budget"
    );
}

/// The composer wraps the raw buffer per character and the finished card
/// word-wraps its markdown, so they only have to agree on the budget. An
/// unbroken token exactly that wide is where a mismatch shows.
#[test]
fn a_draft_and_the_comment_it_becomes_wrap_to_the_same_budget() {
    let width = 48;
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().set_wrap_width(width);
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('c'));
    type_text(
        &mut app,
        &"w".repeat(super::super::super::composer::card_budget(width)),
    );
    let drafted = composer_rows(&app).len();
    app.handle(key('\n'));
    app.diff.as_mut().unwrap().ensure_rows(&app.review);
    let landed = app
        .diff
        .as_ref()
        .unwrap()
        .rows()
        .iter()
        .filter(|row| matches!(row, DiffRow::Comment { .. }))
        .count();
    assert_eq!(drafted, 3, "header, the one full row, footer");
    assert_eq!(
        drafted, landed,
        "writing a comment and reading it back take the same rows"
    );
}

#[test]
fn a_file_level_composer_opens_above_the_diff() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    assert_eq!(focus(&app), Pane::List);
    app.handle(key('c'));
    assert_eq!(
        composer_rows(&app).first().copied(),
        Some(0),
        "a whole-file comment is written where it renders, at the top"
    );
}

#[test]
fn a_reply_composer_opens_under_the_thread_it_answers() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    let line = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = line;
    app.handle(key('c'));
    type_text(&mut app, "question");
    app.handle(key('\n'));
    settle_submit(&mut app);
    app.diff.as_mut().unwrap().cursor = added_line_position(&app) + 1;
    app.handle(key('r'));
    let last_comment = app
        .diff
        .as_ref()
        .unwrap()
        .rows()
        .iter()
        .rposition(|row| matches!(row, DiffRow::Comment { .. }))
        .expect("the thread being replied to");
    assert_eq!(
        composer_rows(&app).first().copied(),
        Some(last_comment + 1),
        "the draft reply sits under the whole block"
    );
    assert!(last_comment > line, "and the block sits under its line");
}

#[test]
fn an_edit_composer_stands_in_for_the_block_it_rewrites() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    let line = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = line;
    app.handle(key('c'));
    type_text(&mut app, "note");
    app.handle(key('\n'));
    settle_submit(&mut app);
    app.diff.as_mut().unwrap().cursor = added_line_position(&app) + 1;
    app.handle(key('c'));
    assert_eq!(composer_rows(&app).first().copied(), Some(line + 1));
    assert!(
        !app.diff
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .any(|row| matches!(row, DiffRow::Comment { .. })),
        "the comment being edited gives up its rows to the draft"
    );
}

#[test]
fn the_pane_cursor_rides_the_caret_so_a_tall_card_scrolls_with_it() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().set_wrap_width(60);
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('c'));
    let caret_row = |app: &App| {
        let diff = app.diff.as_ref().unwrap();
        let caret = diff.composer.as_ref().unwrap().caret_line(diff.wrap_width);
        diff.rows()
            .iter()
            .position(|row| matches!(row, DiffRow::Composer { line } if *line == caret))
    };
    assert_eq!(Some(app.diff.as_ref().unwrap().cursor), caret_row(&app));
    // a card taller than any viewport must keep the cursor on the row the
    // text is landing on, not on the header far above it
    for _ in 0..6 {
        type_text(
            &mut app,
            &"y".repeat(super::super::super::composer::card_budget(60)),
        );
    }
    let rows = composer_rows(&app);
    assert!(rows.len() > 6, "the card grew tall: {}", rows.len());
    assert_eq!(Some(app.diff.as_ref().unwrap().cursor), caret_row(&app));
    assert!(
        app.diff.as_ref().unwrap().cursor > rows[0],
        "the cursor left the header behind"
    );
}

#[test]
fn reopening_the_same_source_keeps_the_draft_on_screen() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    let file = app.diff.as_ref().unwrap().selected;
    assert_ne!(file, 0, "the draft is not on the file a reload defaults to");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('c'));
    type_text(&mut app, "still writing");
    // a queued open landing on the source already in view
    app.open_working_tree_diff(None);
    assert_eq!(
        app.diff
            .as_ref()
            .and_then(|d| d.composer.as_ref())
            .map(|c| c.buffer.clone()),
        Some("still writing".to_owned())
    );
    assert_eq!(app.diff.as_ref().unwrap().selected, file, "same file");
    assert!(
        !composer_rows(&app).is_empty(),
        "an open composer the pane cannot draw would still eat every key"
    );
}

#[test]
fn esc_closes_the_composer_and_gives_its_rows_back() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('c'));
    type_text(&mut app, "never mind");
    app.handle(crate::test_support::esc_key());
    assert!(!app.composer_open());
    assert!(composer_rows(&app).is_empty());
    assert!(app.review.session.comments.is_empty());
}

#[test]
fn a_blank_composer_submits_as_a_cancel() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('c'));
    type_text(&mut app, "   ");
    app.handle(key('\n'));
    assert!(!app.composer_open());
    assert!(app.review.session.comments.is_empty());
}

#[test]
fn c_over_a_comment_edits_it() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    let position = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = position;
    app.handle(key('c'));
    type_text(&mut app, "old note");
    app.handle(key('\n'));

    // then move onto the comment row: `c` edits, prefilled with the body
    settle_submit(&mut app);
    app.diff.as_mut().unwrap().cursor = added_line_position(&app) + 1;
    app.handle(key('c'));
    let composer = app
        .diff
        .as_ref()
        .and_then(|d| d.composer.as_ref())
        .expect("the composer opens over the comment");
    assert_eq!(
        composer.buffer, "old note",
        "prefilled with the existing body"
    );
    // clear and retype
    for _ in 0.."old note".len() {
        app.handle(crate::test_support::key_backspace());
    }
    type_text(&mut app, "new note");
    app.handle(key('\n'));

    assert_eq!(app.review.session.comments.len(), 1, "edited, not added");
    assert_eq!(app.review.session.comments[0].body, "new note");
}

#[test]
fn reply_off_a_comment_row_hints() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    app.handle(key('r'));
    let message = app.message.expect("message");
    assert!(message.text.contains("comment"));
}

#[test]
fn c_in_the_file_list_comments_on_the_whole_file() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    assert_eq!(focus(&app), Pane::List);
    app.handle(key('c'));
    assert!(
        app.composer_open(),
        "c on a file opens a comment, not a hint"
    );
    type_text(&mut app, "whole-file note");
    app.handle(key('\n'));
    let comment = app.review.session.comments.first().expect("a file comment");
    assert_eq!(comment.anchor.line, None, "file-level anchor (no line)");
    assert!(comment.body.contains("whole-file note"));
}

#[test]
fn line_scoped_keys_in_the_list_hint_to_move_into_the_diff() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    assert_eq!(focus(&app), Pane::List);
    app.handle(key('r')); // reply needs a comment row, only in the diff pane
    let message = app.message.expect("message");
    assert!(message.text.contains("move into the diff"));
}
