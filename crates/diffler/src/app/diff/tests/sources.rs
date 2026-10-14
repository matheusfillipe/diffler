//! Tests for diffs of commits and three-dot reviews.

use super::*;

#[test]
fn commit_diff_survives_refresh_untouched() {
    let fixture = standard_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    let oid = app.status.recent[0].oid.clone();
    app.open_commit_diff(&oid);
    assert_eq!(app.screen(), Screen::Diff);
    let before = rows(&app).len();
    fixture.write("zzz.md", "new\n");
    app.handle(ctrl_key('r'));
    assert_eq!(rows(&app).len(), before, "commit model is immutable");
}

#[test]
fn viewed_on_a_commit_diff_persists_to_that_source_not_the_working_tree() {
    let fixture = standard_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    let oid = app.status.recent[0].oid.clone();
    app.open_commit_diff(&oid);
    let path = selected_path(&app);

    // a commit-diff file lives in the pinned commit model, so viewed must
    // resolve against it, not the working-tree model
    app.handle(key('m'));

    let source = ReviewSource::commit(&oid);
    assert!(
        app.review.session_for(&source).viewed.contains_key(&path),
        "viewed mark lands on the commit source"
    );
    assert!(
        app.review.session.viewed.is_empty(),
        "the working-tree session is untouched"
    );

    // a fresh open reloads the commit source's viewed mark from disk
    let mut reopened = App::new(fixture.review(), LoadedConfig::default());
    reopened.open_commit_diff(&oid);
    assert!(
        reopened
            .review
            .session_for(&source)
            .viewed
            .contains_key(&path)
    );
}

#[test]
fn yank_and_editor_on_a_commit_diff_use_that_sources_comments() {
    let fixture = standard_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.review.session.add_comment(
        diffler_core::session::Anchor {
            file: "todo.md".into(),
            line: Some(1),
            line_end: None,
            on_old_side: false,
            line_text: None,
        },
        "reviewer",
        "working-tree decoy",
    );
    let oid = app.status.recent[0].oid.clone();
    app.open_commit_diff(&oid);
    select_file(&mut app, "src/lib.rs");
    let line_row = rows(&app)
        .iter()
        .position(|row| matches!(row, DiffRow::Line { .. }))
        .expect("a diff line in the commit");
    app.diff.as_mut().unwrap().focus = Pane::Diff;
    app.diff.as_mut().unwrap().cursor = line_row;
    app.handle(key('c'));
    type_text(&mut app, "commit note");
    app.handle(key('\n'));

    app.handle(key('Y'));
    let markdown = app.pending_clipboard.take().expect("yanked feedback");
    assert!(markdown.contains("commit note"), "commit comment exported");
    assert!(
        !markdown.contains("working-tree decoy"),
        "working-tree comments stay out of a commit yank"
    );

    let comment_row = rows(&app)
        .iter()
        .position(|row| matches!(row, DiffRow::Comment { .. }))
        .expect("comment row present");
    app.diff.as_mut().unwrap().cursor = comment_row;
    app.handle(key('e'));
    let request = app.pending_editor.take().expect("editor request");
    let argv = request.cmd.join(" ");
    assert!(
        argv.contains("src/lib.rs") && !argv.contains("todo.md"),
        "editor resolves the commit comment, not the working-tree one: {argv}"
    );
}

/// Paths of the open review's diff, in sidebar order.
fn against_paths(app: &App) -> Vec<String> {
    app.diff
        .as_ref()
        .and_then(|diff| diff.commit_model.as_ref())
        .expect("pinned model")
        .files
        .iter()
        .map(|f| f.path.clone())
        .collect()
}

fn against_main(fixture: &Fixture) -> App {
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.handle(key('d'));
    app.handle(key('d'));
    app
}

#[test]
fn the_diff_transient_reviews_the_branch_against_its_base() {
    let fixture = crate::test_support::branch_fixture();
    let app = against_main(&fixture);
    let diff = app.diff.as_ref().expect("diff view");
    assert_eq!(diff.source, ReviewSource::against("main"));
    assert_eq!(app.against_rev(), Some("main"));
    // the branch commit and the uncommitted file, both
    assert_eq!(against_paths(&app), ["dirty.rs", "landed.rs"]);
    assert_eq!(app.screen(), Screen::Diff);
}

#[test]
fn an_against_review_tracks_edits_through_the_refresh() {
    let fixture = crate::test_support::branch_fixture();
    let mut app = against_main(&fixture);
    fixture.write("later.rs", "pub fn later() {}\n");
    app.queue_refresh();
    app.settle_refresh();
    assert_eq!(against_paths(&app), ["dirty.rs", "landed.rs", "later.rs"]);
}

#[test]
fn a_refresh_answering_for_another_rev_leaves_the_open_review_alone() {
    let fixture = crate::test_support::branch_fixture();
    let mut app = against_main(&fixture);
    app.apply_refresh(diffler_core::review::Refreshed {
        status: diffler_core::vcs::StatusModel::default(),
        model: DiffModel::default(),
        against: Some(("develop".to_owned(), Ok(DiffModel::default()))),
        pinned: None,
        head: Ok(app.head.clone()),
        commits: Ok(diffler_core::vcs::CommitLists::default()),
        jj: Ok(None),
    });
    assert_eq!(against_paths(&app), ["dirty.rs", "landed.rs"]);
}

#[test]
fn the_branch_picker_reviews_against_the_chosen_branch() {
    let fixture = crate::test_support::branch_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.handle(key('d'));
    app.handle(key('b'));
    assert!(matches!(app.modal, Some(Modal::RevList { .. })));
    app.handle(code_key(crossterm::event::KeyCode::Tab));
    for c in "main".chars() {
        app.handle(key(c));
    }
    app.handle(key('\n'));
    assert!(app.modal.is_none());
    let diff = app.diff.as_ref().expect("diff view");
    assert_eq!(diff.source, ReviewSource::against("main"));
}

#[test]
fn the_commit_picker_reviews_against_the_chosen_commit() {
    let fixture = crate::test_support::branch_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.handle(key('d'));
    app.handle(key('s'));
    let Some(Modal::RevList { entries, .. }) = &app.modal else {
        panic!("rev picker open, got {:?}", app.modal);
    };
    assert!(entries[0].label.contains("feature work"), "{entries:?}");
    // newest first, so the top entry is HEAD: only the dirty file is over it
    app.handle(key('\n'));
    assert_eq!(against_paths(&app), ["dirty.rs"]);
}

#[test]
fn a_rev_that_does_not_resolve_reports_instead_of_panicking() {
    let fixture = crate::test_support::branch_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.open_against_diff("no-such-branch");
    assert!(app.diff.is_none());
    assert!(
        matches!(&app.message, Some(m) if m.severity == crate::app::Severity::Error),
        "{:?}",
        app.message
    );
}

#[test]
fn w_goes_back_to_the_plain_working_tree_review() {
    let fixture = crate::test_support::branch_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.handle(key('d'));
    app.handle(key('w'));
    let diff = app.diff.as_ref().expect("diff view");
    assert_eq!(diff.source, ReviewSource::WorkingTree);
    assert_eq!(app.against_rev(), None);
}
