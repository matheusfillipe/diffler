use diffler_core::model::LineKind;

use super::*;
use crate::test_support::two_hunk_fixture;

fn cursor_to_changed_line(app: &mut App, kind: LineKind, text: &str) {
    let diff = app.diff.as_mut().expect("diff view");
    diff.ensure_rows(&app.review);
    let model = diff.model(&app.review);
    let row = diff
        .rows()
        .iter()
        .position(|row| {
            let DiffRow::Line { file, hunk, line } = *row else {
                return false;
            };
            let line = &model.files[file].hunks[hunk].lines[line];
            line.kind == kind && line.text == text
        })
        .expect("line on screen");
    let diff = app.diff.as_mut().expect("diff view");
    diff.cursor = row;
    diff.focus = Pane::Diff;
}

fn index_text(app: &App) -> String {
    app.review
        .status
        .staged
        .files
        .iter()
        .find(|f| f.path == "data.txt")
        .and_then(|f| f.new_text.clone())
        .unwrap_or_default()
}

fn staged(app: &App, kind: LineKind, text: &str) -> bool {
    let model = app.review.model();
    let line = model.files[0]
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .find(|line| line.kind == kind && line.text == text)
        .expect("line in the working tree diff");
    app.review.status.staged_lines().is_staged("data.txt", line)
}

#[test]
fn s_stages_the_hunk_under_the_cursor_and_the_rail_knows_it() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "data.txt");
    cursor_to_changed_line(&mut app, LineKind::Added, "line one");

    app.handle(key('s'));
    app.settle_refresh();

    assert!(index_text(&app).starts_with("line one\nline 2\n"));
    assert!(staged(&app, LineKind::Added, "line one"));
    assert!(staged(&app, LineKind::Deleted, "line 1"));
    assert!(!staged(&app, LineKind::Added, "line twenty"));
    assert!(!staged(&app, LineKind::Deleted, "line 20"));
}

#[test]
fn s_on_a_selection_stages_only_those_lines_and_again_takes_them_back() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "data.txt");
    // staging the first hunk shifts nothing, so we also stage a deletion
    // below it to cover the renumbering onto the index
    cursor_to_changed_line(&mut app, LineKind::Added, "line twenty");
    app.handle(key('V'));
    app.handle(key('s'));
    app.settle_refresh();
    assert!(
        index_text(&app).ends_with("line 20\nline twenty\n"),
        "only the added line went in: {}",
        index_text(&app)
    );
    assert!(staged(&app, LineKind::Added, "line twenty"));
    assert!(!staged(&app, LineKind::Deleted, "line 20"));

    cursor_to_changed_line(&mut app, LineKind::Deleted, "line 20");
    app.handle(key('V'));
    app.handle(key('s'));
    app.settle_refresh();
    assert!(index_text(&app).ends_with("line 19\nline twenty\n"));

    cursor_to_changed_line(&mut app, LineKind::Added, "line twenty");
    app.handle(key('s'));
    app.settle_refresh();
    assert!(
        app.review.status.staged.files.is_empty(),
        "a hunk the index holds whole is unstaged"
    );
}

#[test]
fn s_on_a_staged_line_edited_again_replaces_its_staged_copy() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "data.txt");
    cursor_to_changed_line(&mut app, LineKind::Added, "line one");
    app.handle(key('s'));
    app.settle_refresh();
    let text = std::fs::read_to_string(fixture.root.join("data.txt")).expect("read");
    fixture.write("data.txt", &text.replace("line one\n", "line ONE\n"));
    app.handle(AppEvent::RepoChanged);
    app.settle_refresh();

    cursor_to_changed_line(&mut app, LineKind::Added, "line ONE");
    app.handle(key('s'));
    app.settle_refresh();

    assert!(
        index_text(&app).starts_with("line ONE\nline 2\n"),
        "{}",
        index_text(&app)
    );
}

#[test]
fn s_in_the_file_sidebar_stages_the_whole_file_and_again_unstages_it() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "data.txt");
    app.diff.as_mut().expect("diff view").focus = Pane::List;

    app.handle(key('s'));
    app.settle_refresh();
    assert!(app.review.status.unstaged.files.is_empty());
    assert!(staged(&app, LineKind::Added, "line twenty"));

    app.handle(key('s'));
    app.settle_refresh();
    assert!(app.review.status.staged.files.is_empty());
}

#[test]
fn s_outside_the_working_tree_says_why() {
    let fixture = two_hunk_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    let oid = app.status.recent[0].oid.clone();
    app.open_commit_diff(&oid);
    app.handle(key('s'));
    let message = app.message.clone().expect("a message");
    assert!(message.text.contains("working tree"), "{}", message.text);
}
