//! Tests for yanking feedback and selected rows.

use super::*;

#[test]
fn y_copies_the_selected_files_feedback_as_markdown() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    let position = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = position;
    app.handle(key('c'));
    type_text(&mut app, "why 42?");
    app.handle(key('\n'));
    app.review.session.add_comment(
        Anchor {
            file: "todo.md".to_owned(),
            line: None,
            line_end: None,
            on_old_side: false,
            line_text: None,
            on_file: false,
        },
        "reviewer",
        "other file",
    );

    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('y'));
    let payload = app
        .pending_clipboard
        .clone()
        .expect("clipboard text queued");
    let toast = app.message.clone().expect("toast");
    assert_eq!(toast.text, "copied 1 comment (file)");
    let repo = fixture.root.file_name().unwrap().to_string_lossy();
    let expected = feedback::to_markdown(
        &app.review.session,
        app.review.model(),
        &FeedbackOptions {
            title: &format!("Review feedback: {repo} @ main (1 comment)"),
            file_filter: Some("src/lib.rs"),
            include_resolved: false,
        },
    );
    assert!(expected.contains("why 42?"));
    assert!(!expected.contains("other file"), "file filter applies");
    assert_eq!(payload, expected);

    app.handle(key('Y'));
    let message = app.message.clone().expect("toast");
    assert_eq!(message.text, "copied 2 comments (all)");
}

#[test]
fn y_with_a_visual_selection_copies_the_diff_lines() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('V'));
    app.handle(key('y'));
    let text = app
        .pending_clipboard
        .clone()
        .expect("selection copied to the clipboard");
    assert!(
        text.starts_with('+'),
        "added line keeps its marker: {text:?}"
    );
    assert!(text.contains("42"), "the line text is copied: {text:?}");
    assert!(
        !text.contains("  1 "),
        "gutter line numbers are stripped: {text:?}"
    );
    assert_eq!(
        app.diff.as_ref().unwrap().visual_anchor,
        None,
        "the selection clears after copying"
    );
}

#[test]
fn selecting_two_code_lines_yanks_them_with_their_markers() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    cursor_to_line(&mut app, |r| {
        matches!(
            r,
            DiffRow::Line {
                hunk: 0,
                line: 0,
                ..
            }
        )
    });
    app.handle(key('V'));
    app.handle(key('j'));
    app.handle(key('y'));
    let text = app
        .pending_clipboard
        .clone()
        .expect("selection copied to the clipboard");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines, ["-line 1", "+line one"], "{lines:?}");
}

/// A stop with no anchor is prose alone: its card is the whole slide, so
/// selecting it copies the paragraph the reader sees.
#[test]
fn selecting_a_stops_card_yanks_its_prose_lines() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    enter_diff_pane(&mut app);
    app.diff.as_mut().unwrap().cursor = 0;
    app.handle(key('V'));
    app.handle(key('G'));
    app.handle(key('y'));
    let text = app
        .pending_clipboard
        .clone()
        .expect("selection copied to the clipboard");
    assert!(text.contains("the overview"), "{text:?}");
}

#[test]
fn selecting_the_summary_yanks_its_lines() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app_with_summary(&fixture);
    app.diff_tree_to(0);
    enter_diff_pane(&mut app);
    app.diff.as_mut().unwrap().cursor = 0;
    app.handle(key('V'));
    app.handle(key('G'));
    app.handle(key('y'));
    let text = app
        .pending_clipboard
        .clone()
        .expect("selection copied to the clipboard");
    assert!(text.contains("the shape of the change"), "{text:?}");
}

/// A whole-file comment renders above the diff, so a selection that
/// starts on its card and runs into the first code line copies both,
/// the card first, the same order the pane shows them in.
#[test]
fn selecting_across_a_card_and_the_code_below_it_yanks_both_in_order() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.review.session.add_comment(
        Anchor {
            file: "src/lib.rs".to_owned(),
            line: None,
            line_end: None,
            on_old_side: false,
            line_text: None,
            on_file: false,
        },
        "reviewer",
        "why this file changed",
    );
    app.diff.as_mut().unwrap().invalidate();
    app.diff.as_mut().unwrap().ensure_rows(&app.review);
    let first_line = rows(&app)
        .iter()
        .position(|r| matches!(r, DiffRow::Line { .. }))
        .expect("a code line follows the card");
    app.diff.as_mut().unwrap().cursor = 0;
    app.handle(key('V'));
    app.diff.as_mut().unwrap().cursor = first_line;
    app.handle(key('y'));
    let text = app
        .pending_clipboard
        .clone()
        .expect("selection copied to the clipboard");
    let lines: Vec<&str> = text.lines().collect();
    let card_at = lines
        .iter()
        .position(|line| line.contains("why this file changed"))
        .expect("the card's prose is copied");
    let code_at = lines
        .iter()
        .position(|line| line.starts_with('+') || line.starts_with('-') || line.starts_with(' '))
        .expect("a code line is copied");
    assert!(
        card_at < code_at,
        "the card reads before the code under it: {lines:?}"
    );
}

#[test]
fn y_with_no_selection_still_exports_comments_as_markdown() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.review.session.add_comment(
        Anchor {
            file: "src/lib.rs".to_owned(),
            line: None,
            line_end: None,
            on_old_side: false,
            line_text: None,
            on_file: false,
        },
        "reviewer",
        "why 42?",
    );
    app.diff.as_mut().unwrap().invalidate();
    assert!(
        app.diff.as_ref().unwrap().visual_anchor.is_none(),
        "no selection is active"
    );
    app.handle(key('y'));
    let text = app
        .pending_clipboard
        .clone()
        .expect("clipboard text queued");
    assert!(text.contains("why 42?"), "exported as markdown: {text:?}");
}

#[test]
fn yanking_a_selection_drops_the_anchor() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
    cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
    app.handle(key('V'));
    app.handle(key('y'));
    assert!(
        app.diff.as_ref().unwrap().visual_anchor.is_none(),
        "the yank drops the anchor"
    );
}

#[test]
fn y_with_no_comments_hints_instead_of_copying() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    // feedback copy lives in the diff pane; the sidebar yanks paths
    app.diff.as_mut().unwrap().focus = Pane::Diff;
    app.handle(key('y'));
    assert_eq!(app.pending_clipboard, None);
    let message = app.message.expect("message");
    assert!(message.text.contains("no comments"));
}

#[test]
fn yank_in_the_sidebar_copies_the_path_the_row_names() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    app.diff.as_mut().unwrap().focus = Pane::List;
    app.handle(key('y'));
    let path = app
        .diff
        .as_ref()
        .and_then(|diff| {
            let model = diff.model(&app.review);
            model.files.get(diff.selected).map(|file| file.path.clone())
        })
        .expect("a file under the cursor");
    assert_eq!(app.pending_clipboard.as_deref(), Some(path.as_str()));
    assert!(
        app.message
            .as_ref()
            .is_some_and(|m| !m.text.contains("no comments")),
        "the sidebar never reaches the feedback copy"
    );
}
