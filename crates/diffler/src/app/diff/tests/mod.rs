//! Tests for the diff screen, split by topic, with the helpers they share.

mod comments;
mod composer;
mod navigation;
mod refresh;
mod side_by_side;
mod sidebar;
mod sources;
mod viewed;
mod walkthrough;
mod yank;

use diffler_core::feedback::{self, FeedbackOptions};
use diffler_core::model::LineKind;
use diffler_core::session::{Anchor, CommentStatus, Session};
use unicode_width::UnicodeWidthStr;

use crate::app::markdown::MdSpan;

use super::sidebar::sidebar_rows;
use super::*;
use crate::app::{App, Modal, Screen};
use crate::config::LoadedConfig;
use crate::event::AppEvent;
use crate::test_support::{
    Fixture, big_file_fixture, code_key, ctrl_key, huge_span_fixture, key, settle_submit,
    standard_fixture, two_hunk_fixture,
};

fn diff_app(fixture: &Fixture) -> App {
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.author = "reviewer".to_owned();
    app.open_working_tree_diff(None);
    app
}

/// A diff app whose sidebar layout is forced to `layout`, overriding the
/// default tree.
fn diff_app_with_layout(fixture: &Fixture, layout: crate::config::FileLayout) -> App {
    let mut loaded = LoadedConfig::default();
    loaded.config.ui.diff_file_layout = layout;
    let mut app = App::new(fixture.review(), loaded);
    app.author = "reviewer".to_owned();
    app.open_working_tree_diff(None);
    app
}

fn rows(app: &App) -> Vec<DiffRow> {
    app.diff.as_ref().expect("diff view").rows().to_vec()
}

fn focus(app: &App) -> Pane {
    app.diff.as_ref().expect("diff view").focus
}

fn selected_path(app: &App) -> String {
    app.diff
        .as_ref()
        .expect("diff view")
        .selected_path(&app.review)
        .expect("selected file")
}

fn tree_cursor(app: &App) -> usize {
    app.diff.as_ref().expect("diff view").tree_cursor
}

fn tree_row_count(app: &App) -> usize {
    let diff = app.diff.as_ref().expect("diff view");
    sidebar_rows(diff, &app.review).len()
}

/// Kinds of the visible sidebar tree rows: "dir", "file:<name>",
/// "section:<label>:<count>", or "stop:<index>".
fn tree_kinds(app: &App) -> Vec<String> {
    let diff = app.diff.as_ref().expect("diff view");
    sidebar_rows(diff, &app.review)
        .iter()
        .map(|row| match &row.node {
            crate::tree::TreeNode::Dir { .. } => "dir".to_owned(),
            crate::tree::TreeNode::File { name, .. } => format!("file:{name}"),
            crate::tree::TreeNode::Section { bucket, count, .. } => {
                format!("section:{}:{count}", bucket.label())
            }
            crate::tree::TreeNode::Stop { index } => format!("stop:{index}"),
            crate::tree::TreeNode::WalkthroughSummary => "walkthrough_summary".to_owned(),
        })
        .collect()
}

/// Put focus on the diff pane (unscoped open starts on the sidebar).
fn enter_diff_pane(app: &mut App) {
    app.diff.as_mut().expect("diff view").focus = Pane::Diff;
}

fn cursor_to_line(app: &mut App, pred: impl Fn(&DiffRow) -> bool) {
    let position = rows(app).iter().position(pred).expect("row present");
    app.diff.as_mut().unwrap().cursor = position;
}

/// Select the file at `path`, then focus the diff pane.
fn select_file(app: &mut App, path: &str) {
    let index = app
        .diff
        .as_ref()
        .unwrap()
        .model(&app.review)
        .files
        .iter()
        .position(|f| f.path == path)
        .expect("file present");
    app.diff_select_file_index(index);
    enter_diff_pane(app);
}

/// Row index of the first added line ("42") in the standard fixture's
/// src/lib.rs diff.
fn added_line_position(app: &App) -> usize {
    let diff = app.diff.as_ref().unwrap();
    let model = diff.model(&app.review);
    rows(app)
        .iter()
        .position(|row| {
            let DiffRow::Line { file, hunk, line } = row else {
                return false;
            };
            model.files.get(*file).is_some_and(|f| {
                f.path == "src/lib.rs"
                    && f.hunks.get(*hunk).is_some_and(|h| {
                        h.lines
                            .get(*line)
                            .is_some_and(|l| l.new_no.is_some() && l.text.contains("42"))
                    })
            })
        })
        .expect("added line present")
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle(key(c));
    }
}

/// Row of the open composer's own caret, the row `seat_cursor_on_caret`
/// parks the pane cursor on while a reader is typing.
fn caret_row(app: &App) -> usize {
    let diff = app.diff.as_ref().unwrap();
    let composer = diff.composer.as_ref().expect("composer open");
    let caret = composer.caret_line(diff.wrap_width);
    diff.rows()
        .iter()
        .position(|row| matches!(row, DiffRow::Composer { line } if *line == caret))
        .expect("the caret's own row")
}

/// Three stops over the standard fixture: two files, one of them named
/// twice, and one stop with no anchor at all.
fn seat_walkthrough(app: &mut App) {
    crate::test_support::seat_walkthrough(
        app,
        "How the answer moved",
        &[
            ("What changed", None, "the overview"),
            ("The list", Some("todo.md:1"), "why a list"),
            ("The answer", Some("src/lib.rs#answer"), "why 42"),
        ],
    );
}

fn walkthrough_app(fixture: &Fixture) -> App {
    let mut app = crate::test_support::walkthrough_layout_app(fixture);
    seat_walkthrough(&mut app);
    app.open_walkthrough_diff("w1");
    resolve_walkthrough(&mut app);
    app
}

/// Like `walkthrough_app`, with a summary set before the diff opens, so
/// its row appears in the sidebar and the initial seat lands past it.
fn walkthrough_app_with_summary(fixture: &Fixture) -> App {
    let mut app = crate::test_support::walkthrough_layout_app(fixture);
    seat_walkthrough(&mut app);
    crate::test_support::set_walkthrough_summary(&mut app, "w1", "the shape of the change");
    app.open_walkthrough_diff("w1");
    resolve_walkthrough(&mut app);
    app
}

/// A walkthrough app over an arbitrary fixture with an arbitrary stop
/// list, resolved the way the worker would. Real usage always rebuilds
/// rows on the next key or render after a resolution lands; a test that
/// reads rows with neither has to ask explicitly.
fn walkthrough_app_with(fixture: &Fixture, stops: &[(&str, Option<&str>, &str)]) -> App {
    let mut app = crate::test_support::walkthrough_layout_app(fixture);
    crate::test_support::seat_walkthrough(&mut app, "tour", stops);
    app.open_walkthrough_diff("w1");
    resolve_walkthrough(&mut app);
    app.diff.as_mut().unwrap().ensure_rows(&app.review);
    app
}

/// Read the anchored files the way the worker would: the live worktree,
/// since a walkthrough always tracks it.
fn resolve_walkthrough(app: &mut App) {
    let Some(request) = app.pending_walkthrough.take() else {
        return;
    };
    let root = app.review.repo_root.clone();
    let contents = request
        .files
        .iter()
        .filter_map(|path| Some((path.clone(), std::fs::read_to_string(root.join(path)).ok()?)))
        .collect();
    app.handle(AppEvent::WalkthroughAnchors {
        contents,
        pin_broken: false,
        token: request.token,
    });
}

/// Put the sidebar cursor on the tree row for `dir`.
fn cursor_to_dir(app: &mut App, dir: &str) {
    let review = &app.review;
    let diff = app.diff.as_ref().expect("diff view");
    let rows = super::sidebar_rows(diff, review);
    let at = rows
        .iter()
        .position(|row| matches!(&row.node, crate::tree::TreeNode::Dir { path, .. } if path == dir))
        .unwrap_or_else(|| panic!("a tree row for {dir}"));
    let diff = app.diff.as_mut().expect("diff view");
    diff.focus = Pane::List;
    diff.tree_cursor = at;
}

fn viewed_paths(app: &App) -> Vec<String> {
    let source = app.active_review_source();
    let session = app.review.session_for(&source);
    let mut out: Vec<String> = app
        .diff
        .as_ref()
        .unwrap()
        .model(&app.review)
        .files
        .iter()
        .filter(|f| session.is_viewed(&f.path, &f.content_hash()))
        .map(|f| f.path.clone())
        .collect();
    out.sort();
    out
}

/// Two files directly under `src/`, one nested a level deeper, and one
/// outside it entirely, so a subtree mark has something real to cover and
/// something to leave alone.
fn nested_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn a() {}\n");
    fixture.write("src/main.rs", "fn main() {}\n");
    fixture.write("src/deep/inner.rs", "pub fn b() {}\n");
    fixture.write("docs/readme.md", "hi\n");
    fixture.commit_all("initial");
    fixture.write("src/lib.rs", "pub fn a() -> u32 {\n    1\n}\n");
    fixture.write("src/main.rs", "fn main() {\n    println!();\n}\n");
    fixture.write("src/deep/inner.rs", "pub fn b() -> u32 {\n    2\n}\n");
    fixture.write("docs/readme.md", "hi there\n");
    fixture
}

/// Put the sidebar cursor on the section header for `label`.
fn cursor_to_section(app: &mut App, label: &str) {
    let review = &app.review;
    let diff = app.diff.as_ref().expect("diff view");
    let rows = super::sidebar_rows(diff, review);
    let at = rows
        .iter()
        .position(|row| {
            matches!(&row.node, crate::tree::TreeNode::Section { bucket, .. }
                if bucket.label() == label)
        })
        .unwrap_or_else(|| panic!("a section row for {label}: {rows:?}"));
    let diff = app.diff.as_mut().expect("diff view");
    diff.focus = Pane::List;
    diff.tree_cursor = at;
}

/// One source file and two the built-in rules call Generated (a lockfile
/// and a snapshot), so a kind header stands for more than one file.
fn kinds_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", "pub fn a() {}\n");
    fixture.write("tests/snapshots/render.snap", "old frame\n");
    fixture.write("package-lock.json", "{\n  \"lockfileVersion\": 3\n}\n");
    fixture.commit_all("initial");
    fixture.write("src/lib.rs", "pub fn a() -> u32 {\n    1\n}\n");
    fixture.write("tests/snapshots/render.snap", "new frame\n");
    fixture.write("package-lock.json", "{\n  \"lockfileVersion\": 4\n}\n");
    fixture
}

fn tree_cursor_row(app: &App) -> String {
    let diff = app.diff.as_ref().expect("diff view");
    let rows = super::sidebar_rows(diff, &app.review);
    match &rows[diff.tree_cursor].node {
        crate::tree::TreeNode::Dir { path, .. } => format!("dir {path}"),
        crate::tree::TreeNode::Section { bucket, .. } => format!("section {}", bucket.label()),
        crate::tree::TreeNode::File { name, .. } => format!("file {name}"),
        crate::tree::TreeNode::Stop { index } => format!("stop {index}"),
        crate::tree::TreeNode::WalkthroughSummary => "walkthrough_summary".to_owned(),
    }
}

/// Four files in one group, so marking has somewhere to sort them to.
fn flat_fixture() -> Fixture {
    let fixture = Fixture::new();
    for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
        fixture.write(name, "fn x() {}\n");
    }
    fixture.commit_all("base");
    for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
        fixture.write(name, "fn x() -> u32 {\n    1\n}\n");
    }
    fixture
}

/// Three files in one directory, so a group has an order to sort.
fn grouped_fixture() -> Fixture {
    let fixture = Fixture::new();
    for name in ["a.rs", "b.rs", "c.rs"] {
        fixture.write(&format!("src/{name}"), "fn one() {}\n");
    }
    fixture.commit_all("base");
    for name in ["a.rs", "b.rs", "c.rs"] {
        fixture.write(&format!("src/{name}"), "fn one() -> u8 {\n    1\n}\n");
    }
    fixture
}

fn fixture_content(app: &crate::app::App) -> String {
    app.review
        .model()
        .files
        .iter()
        .find(|f| f.path == "src/lib.rs")
        .and_then(|f| f.new_text.clone())
        .expect("new text")
}

fn current_hash(app: &crate::app::App) -> String {
    app.review
        .model()
        .files
        .iter()
        .find(|f| f.path == "src/lib.rs")
        .map(diffler_core::model::FileDiff::sides_hash)
        .expect("hash")
}
