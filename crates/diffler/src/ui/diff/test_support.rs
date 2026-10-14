//! Fixtures shared by the diff screen's tests.

use crate::app::{App, DiffRow, Pane};
use crate::config::LoadedConfig;
use crate::test_support::{Fixture, standard_fixture};
use ratatui::text::Line;

pub(super) fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

pub(super) fn plain(text: &str) -> Vec<ratatui::text::Span<'static>> {
    vec![ratatui::text::Span::raw(text.to_owned())]
}

pub(super) fn diff_app() -> (crate::test_support::Fixture, App) {
    let fixture = standard_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.author = "reviewer".to_owned();
    app.open_working_tree_diff(None);
    (fixture, app)
}

pub(super) fn walkthrough_app(fixture: &Fixture, stops: &[(&str, Option<&str>, &str)]) -> App {
    let mut loaded = LoadedConfig::default();
    loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
    let mut app = App::new(fixture.review(), loaded);
    app.author = "reviewer".to_owned();
    crate::test_support::seat_walkthrough(&mut app, "How the answer moved", stops);
    app.open_walkthrough_diff("w1");
    // anchors resolve off-thread, so we land them before rendering
    let Some(request) = app.pending_walkthrough.take() else {
        return app;
    };
    let root = app.review.repo_root.clone();
    let read = diffler_core::review::Review::compute_walkthrough_files(
        &root,
        request.read_rev.as_deref(),
        request.read_first,
        &request.files,
    );
    app.handle(crate::event::AppEvent::WalkthroughAnchors {
        contents: read.contents,
        pin_broken: read.pin_broken,
        token: request.token,
    });
    app
}

/// Select src/lib.rs and focus the diff pane.
pub(super) fn open_lib_diff(app: &mut App) {
    let index = app
        .diff
        .as_ref()
        .unwrap()
        .model(&app.review)
        .files
        .iter()
        .position(|f| f.path == "src/lib.rs")
        .expect("src/lib.rs present");
    let diff = app.diff.as_mut().unwrap();
    diff.selected = index;
    diff.focus = Pane::Diff;
    diff.invalidate();
    diff.ensure_rows(&app.review);
}

pub(super) fn cursor_to_added_line(app: &mut App) {
    let diff = app.diff.as_ref().unwrap();
    let model = diff.model(&app.review);
    let position = diff
        .rows()
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
        .expect("added line");
    app.diff.as_mut().unwrap().cursor = position;
}

/// The first two `DiffRow::Line` rows and their on-screen y positions.
pub(super) fn first_two_pane_lines(app: &App) -> (u16, u16, u16, usize, usize) {
    let diff = app.diff.as_ref().unwrap();
    let lines: Vec<usize> = diff
        .rows()
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, DiffRow::Line { .. }))
        .map(|(i, _)| i)
        .collect();
    let x = diff.pane.x + 1;
    let y0 = diff.pane.y + lines[0] as u16 - diff.scroll as u16;
    let y1 = diff.pane.y + lines[1] as u16 - diff.scroll as u16;
    (x, y0, y1, lines[0], lines[1])
}
