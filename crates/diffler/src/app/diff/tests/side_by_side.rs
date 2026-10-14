//! Tests for the side-by-side layout.

use super::*;

#[test]
fn build_split_rows_aligns_old_and_new_sides() {
    let fixture = standard_fixture();
    let app = diff_app(&fixture);
    let diff = app.diff.as_ref().unwrap();
    let model = diff.model(&app.review);
    let session = app.review.session_for(&ReviewSource::WorkingTree);
    for (index, file) in model.files.iter().enumerate() {
        for row in build_split_rows(model, session, index, u16::MAX, None, diff.card_views()) {
            let SplitRow::Pair { hunk, left, right } = row else {
                continue;
            };
            assert!(left.is_some() || right.is_some(), "a pair fills a side");
            let lines = &file.hunks[hunk].lines;
            if let Some(l) = left {
                assert!(matches!(
                    lines[l].kind,
                    LineKind::Deleted | LineKind::Context
                ));
            }
            if let Some(r) = right {
                assert!(matches!(lines[r].kind, LineKind::Added | LineKind::Context));
            }
            if left == right {
                assert!(matches!(lines[left.unwrap()].kind, LineKind::Context));
            }
        }
    }
}

#[test]
fn toggle_side_by_side_flips_the_mode() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    app.diff.as_mut().unwrap().focus = Pane::Diff;
    assert!(!app.diff.as_ref().unwrap().side_by_side);
    app.handle(key('|'));
    assert!(app.diff.as_ref().unwrap().side_by_side);
    app.handle(key('|'));
    assert!(!app.diff.as_ref().unwrap().side_by_side);
}

#[test]
fn commenting_in_side_by_side_is_redirected_to_unified() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    let diff = app.diff.as_mut().unwrap();
    diff.focus = Pane::Diff;
    diff.side_by_side = true;
    app.handle(key('c'));
    assert!(app.modal.is_none(), "no comment modal opens in split mode");
    assert!(
        app.message
            .as_ref()
            .is_some_and(|m| m.text.contains("unified")),
        "the message points to the unified view"
    );
}
