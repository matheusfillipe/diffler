//! The whole-file view. The blame column takes the left edge and collapses a
//! commit's run to a single row, so a block of untouched code reads as one
//! attribution.

use diffler_core::highlight::StyledRange;
use ratatui::Frame;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::app::file::FileView;
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
    view.scroll = super::scroll_to_cursor(view.cursor, view.scroll, height, view.lines.len());
    let gutter = view.lines.len().to_string().len().max(3);
    let lines: Vec<Line<'static>> = (view.scroll..view.lines.len())
        .take(height)
        .map(|index| {
            let ranges = search
                .as_ref()
                .and_then(|all| all.get(index).cloned())
                .unwrap_or_default();
            let line = row_line(&theme, view, index, gutter, body.width, &ranges, now);
            if index == view.cursor {
                cursor_line(line, &theme, body.width)
            } else {
                line
            }
        })
        .collect();
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
) -> Line<'static> {
    // the cursor rail overwrites the first cell, so every row opens with one
    // the content can afford to lose
    let mut spans = vec![Span::raw(" ")];
    if view.show_blame {
        spans.push(blame_span(theme, view, index, now));
    }
    spans.push(Span::styled(
        format!("{:>gutter$} ", index + 1),
        theme.dim_style(),
    ));
    let text = view.lines.get(index).cloned().unwrap_or_default();
    let syntax = view.highlights.get(index).map(Vec::as_slice);
    spans.extend(text_spans(theme, &text, syntax, search));
    let used: usize = spans.iter().map(Span::width).sum();
    if let Some(pad) = (width as usize).checked_sub(used) {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    Line::from(spans)
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

/// Syntax foregrounds over the line, with search hits taking a highlight bg.
fn text_spans(
    theme: &Theme,
    text: &str,
    syntax: Option<&[StyledRange]>,
    search: &[(std::ops::Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let bg_at = |byte: usize| {
        search
            .iter()
            .find(|(range, _)| range.contains(&byte))
            .map(|(_, current)| {
                if *current {
                    theme.search_current
                } else {
                    theme.search
                }
            })
    };
    let fg_at = |byte: usize| {
        syntax
            .unwrap_or_default()
            .iter()
            .find(|styled| styled.range.contains(&byte))
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current: Option<(Style, String)> = None;
    for (byte, ch) in text.char_indices() {
        let styled = fg_at(byte);
        let mut style = Style::new().fg(styled.map_or(theme.fg, |s| {
            let (r, g, b) = s.fg;
            Color::Rgb(r, g, b)
        }));
        if styled.is_some_and(|s| s.bold) {
            style = style.add_modifier(ratatui::style::Modifier::BOLD);
        }
        if styled.is_some_and(|s| s.italic) {
            style = style.add_modifier(ratatui::style::Modifier::ITALIC);
        }
        if let Some(bg) = bg_at(byte) {
            style = style.bg(bg);
        }
        match current.as_mut() {
            Some((open, buffer)) if *open == style => buffer.push(ch),
            _ => {
                if let Some((open, buffer)) = current.take() {
                    spans.push(Span::styled(buffer, open));
                }
                current = Some((style, ch.to_string()));
            }
        }
    }
    if let Some((open, buffer)) = current {
        spans.push(Span::styled(buffer, open));
    }
    spans
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
