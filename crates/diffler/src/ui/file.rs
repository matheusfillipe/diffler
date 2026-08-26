//! The whole-file view. The blame column takes the left edge and collapses a
//! commit's run to a single row, so a block of untouched code reads as one
//! attribution.

use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::app::file::FileView;
use crate::app::rowsel::RowSelect;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::{Hint, cursor_line, status_bar};

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::ToggleBlame], "blame"),
    Hint::Leaf(&[Action::Open], "commit"),
    Hint::Leaf(&[Action::OpenEditor], "editor"),
    Hint::Leaf(&[Action::Help], "help"),
];

/// Width of the blame column: 7 for the sha, a space, 12 for the author, two
/// spaces, 4 for the age, and a trailing separator space.
const BLAME_WIDTH: usize = 27;
const AUTHOR_WIDTH: usize = 12;

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (header, body, bar) = super::screen_chrome_with_header(frame, app, HINTS);
    let theme = app.theme.clone();
    let now = app.now_unix;
    let search = app.search.as_ref().map(|s| {
        (0..app.file.as_ref().map_or(0, |v| v.lines.len()))
            .map(|index| s.ranges_for(index))
            .collect::<Vec<_>>()
    });

    let Some(view) = app.file.as_mut() else {
        frame.render_widget(
            Paragraph::new(Line::styled("  loading…", theme.dim_style())),
            body,
        );
        frame.render_widget(
            Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
            bar,
        );
        return;
    };
    view.viewport = body.height;

    frame.render_widget(Paragraph::new(header_line(view, &theme)), header);

    let height = body.height.max(1) as usize;
    let gutter = view.lines.len().to_string().len().max(3);
    let blame_cols = if view.show_blame { BLAME_WIDTH } else { 0 };
    let prefix_cols = 1 + blame_cols + gutter + 1;
    // a wrapped line owns several terminal rows, so scrolling counts rows, not
    // source lines; heights are counted by the same greedy walk that wraps
    let row_height = |index: usize| {
        view.lines.get(index).map_or(1, |text| {
            super::diff_render::text_height(text, prefix_cols, body.width)
        })
    };
    let cursor_row: usize = (0..view.cursor).map(row_height).sum();
    let total: usize = (0..view.lines.len()).map(row_height).sum();
    view.scroll = super::scroll_to_span(
        cursor_row,
        row_height(view.cursor),
        view.scroll,
        height,
        total,
    );

    // walk source lines until the viewport is full, skipping the rows above it
    let selected = |index: usize| view.row_selected(index);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row = 0usize;
    for index in 0..view.lines.len() {
        let span = row_height(index);
        if row + span <= view.scroll {
            row += span;
            continue;
        }
        let ranges = search
            .as_ref()
            .and_then(|all| all.get(index).cloned())
            .unwrap_or_default();
        let mut rendered = row_line(&theme, view, index, gutter, body.width, &ranges, now);
        if selected(index) {
            rendered = rendered
                .into_iter()
                .map(|line| cursor_line(line, &theme, body.width))
                .collect();
        }
        for (offset, line) in rendered.into_iter().enumerate() {
            if row + offset >= view.scroll {
                lines.push(line);
            }
        }
        row += span;
        if lines.len() >= height {
            break;
        }
    }
    lines.truncate(height);
    frame.render_widget(Paragraph::new(lines), body);

    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

fn header_line(view: &FileView, theme: &Theme) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("  {}", view.path),
        Style::new().fg(theme.accent),
    )];
    spans.push(Span::styled(
        format!("  {} lines", view.lines.len()),
        theme.dim_style(),
    ));
    if let Some(span) = view.cursor_commit() {
        spans.push(Span::styled(
            format!("  ▸ {} {}", span.oid7, span.summary),
            theme.dim_style(),
        ));
    }
    Line::from(spans)
}

fn row_line(
    theme: &Theme,
    view: &FileView,
    index: usize,
    gutter: usize,
    width: u16,
    search: &[(std::ops::Range<usize>, bool)],
    now: i64,
) -> Vec<Line<'static>> {
    let text = view.lines.get(index).cloned().unwrap_or_default();
    let syntax = view.highlights.get(index).map(Vec::as_slice);
    let bg = theme.bg;
    // the same compositing the diff pane uses, so syntax and search hits look
    // identical in both, and the same wrapper, so long lines wrap the same way
    let content =
        crate::ui::diff_render::composite_spans(theme, &text, &[], syntax, bg, bg, search);
    let blame_cols = if view.show_blame { BLAME_WIDTH } else { 0 };
    // the cursor rail overwrites the first cell, so every row opens with one
    // the content can afford to lose
    let prefix_cols = 1 + blame_cols + gutter + 1;
    let prefix = |first: bool| {
        let mut spans = vec![Span::raw(" ")];
        if view.show_blame {
            spans.push(if first {
                blame_span(theme, view, index, now)
            } else {
                Span::raw(" ".repeat(BLAME_WIDTH))
            });
        }
        spans.push(Span::styled(
            if first {
                format!("{:>gutter$} ", index + 1)
            } else {
                " ".repeat(gutter + 1)
            },
            theme.dim_style(),
        ));
        spans
    };
    crate::ui::diff_render::wrapped_rows(content, prefix, prefix_cols, width, bg)
}

/// One blame cell. Only the first line of a commit's run prints it; the rest
/// of the run is blank, which is what makes a long untouched block readable.
fn blame_span(theme: &Theme, view: &FileView, index: usize, now: i64) -> Span<'static> {
    if !view.starts_span(index) {
        return Span::raw(" ".repeat(BLAME_WIDTH));
    }
    let Some(span) = view.span_at(index) else {
        return Span::raw(" ".repeat(BLAME_WIDTH));
    };
    if !span.committed {
        return Span::styled(
            format!("{:<BLAME_WIDTH$}", "  uncommitted"),
            theme.dim_style(),
        );
    }
    let author = super::elide(&span.author, AUTHOR_WIDTH);
    let text = format!(
        "{} {:<AUTHOR_WIDTH$} {:>4} ",
        span.oid7,
        author,
        super::relative_time(now, span.time_unix)
    );
    Span::styled(format!("{text:<BLAME_WIDTH$}"), Style::new().fg(theme.dim))
}

#[cfg(test)]
mod tests {
    use diffler_core::vcs::BlameSpan;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    fn span(start: u32, count: u32, oid7: &str, author: &str, summary: &str) -> BlameSpan {
        BlameSpan {
            start_line: start,
            line_count: count,
            oid: format!("{oid7}0000000000000000000000000000000"),
            oid7: oid7.to_owned(),
            author: author.to_owned(),
            time_unix: 0,
            summary: summary.to_owned(),
            committed: true,
        }
    }

    fn app_with_file(show_blame: bool) -> App {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.now_unix = 86_400 * 30;
        let view = FileView::new(
            "src/lib.rs".to_owned(),
            "fn main() {\n    let total = 1;\n    println!(\"{total}\");\n}\n",
            Vec::new(),
            vec![
                span(1, 2, "aaaaaaa", "reviewer", "first"),
                span(3, 2, "bbbbbbb", "other", "second"),
            ],
            show_blame,
        );
        app.install_file(view, None);
        app
    }

    #[test]
    fn renders_the_file_with_its_blame_column() {
        let mut app = app_with_file(true);
        let backend = TestBackend::new(72, 9);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn a_visual_selection_tints_every_line_it_covers() {
        let mut app = app_with_file(true);
        app.handle(crate::test_support::key('V'));
        app.handle(crate::test_support::key('j'));
        assert_eq!(
            app.file.as_ref().and_then(FileView::selection),
            Some((0, 1))
        );
        let paint = |app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(72, 9)).expect("terminal");
            terminal.draw(|f| draw(f, app)).expect("draw");
            terminal
        };
        let bg = format!("{:?}", app.theme.cursor_line);
        let terminal = paint(&mut app);
        let selected = format!("{:?}", terminal.backend().buffer())
            .matches(&bg)
            .count();
        app.file.as_mut().expect("a file view").visual_anchor = None;
        let unselected = format!("{:?}", paint(&mut app).backend().buffer())
            .matches(&bg)
            .count();
        assert!(
            selected > unselected,
            "the selection must paint past the cursor row: {selected} vs {unselected}"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn a_long_line_wraps_under_a_blank_gutter() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.now_unix = 86_400 * 30;
        let long = "let total = ".to_owned() + &"x".repeat(90) + ";";
        let view = FileView::new(
            "src/lib.rs".to_owned(),
            &format!("fn main() {{\n    {long}\n}}\n"),
            Vec::new(),
            vec![span(1, 3, "aaaaaaa", "reviewer", "first")],
            true,
        );
        app.install_file(view, None);
        let backend = TestBackend::new(72, 9);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_without_the_blame_column_when_toggled_off() {
        let mut app = app_with_file(false);
        let backend = TestBackend::new(72, 9);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    fn spans(count: u32, start: u32) -> Vec<BlameSpan> {
        vec![BlameSpan {
            start_line: start,
            line_count: count,
            oid: "a".repeat(40),
            oid7: "aaaaaaa".to_owned(),
            author: "reviewer".to_owned(),
            time_unix: 0,
            summary: "base".to_owned(),
            committed: true,
        }]
    }

    #[test]
    fn only_the_first_line_of_a_run_prints_its_commit() {
        let view = FileView::new(
            "a.rs".to_owned(),
            "one\ntwo\nthree\n",
            Vec::new(),
            spans(3, 1),
            true,
        );
        let theme = Theme::github_dark();
        assert!(blame_span(&theme, &view, 0, 0).content.contains("aaaaaaa"));
        assert_eq!(blame_span(&theme, &view, 1, 0).content.trim(), "");
        assert_eq!(blame_span(&theme, &view, 2, 0).content.trim(), "");
    }
}
