//! The CI job-log view: the `gh ... --log` output as collapsible steps. Each
//! step is a header row (▾/▸) with its lines underneath; folded by default. The
//! screen reuses the diff/log keymap, so motions, search, visual select, and
//! yank all behave as elsewhere.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::app::logs::{LogsRow, LogsView};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::{Hint, cursor_line, hint_line, status_bar};

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::ToggleFold], "fold"),
    Hint::Leaf(&[Action::MoveDown, Action::MoveUp], "move"),
    Hint::Leaf(&[Action::Search], "search"),
    Hint::Leaf(&[Action::CopyFileFeedback], "yank"),
    Hint::Leaf(&[Action::Back], "back"),
];

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, body, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(hint_line(app, HINTS)), hint);

    let search = app.search.as_ref();
    match app.logs.as_mut() {
        Some(view) if !view.steps.is_empty() => {
            view.viewport = body.height;
            view.body = body;
            let height = body.height.max(1) as usize;
            let rows = view.rows();
            view.scroll = super::scroll_to_cursor(view.cursor, view.scroll, height);
            let selection = view.selection();
            let selected =
                |i: usize| i == view.cursor || selection.is_some_and(|(lo, hi)| i >= lo && i <= hi);
            let lines: Vec<Line<'static>> = rows
                .iter()
                .enumerate()
                .skip(view.scroll)
                .take(height)
                .map(|(index, row)| {
                    let ranges = search.map(|s| s.ranges_for(index)).unwrap_or_default();
                    let line = row_line(&app.theme, view, *row, &ranges);
                    if selected(index) {
                        cursor_line(line, &app.theme, body.width)
                    } else {
                        line
                    }
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), body);
        }
        _ => {
            frame.render_widget(
                Paragraph::new(Line::styled("  waiting for logs…", app.theme.dim_style())),
                body,
            );
        }
    }

    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

fn row_line(
    theme: &Theme,
    view: &LogsView,
    row: LogsRow,
    search: &[(std::ops::Range<usize>, bool)],
) -> Line<'static> {
    match row {
        LogsRow::Step(s) => {
            let Some(step) = view.steps.get(s) else {
                return Line::default();
            };
            let marker = if step.folded { "▸" } else { "▾" };
            let mut spans = vec![Span::styled(
                format!(" {marker} "),
                Style::new().fg(theme.accent),
            )];
            if step.name.is_empty() {
                spans.push(Span::styled("log", theme.dim_style()));
            } else {
                spans.extend(super::highlight_spans(
                    &step.name,
                    Style::new().fg(theme.fg).bg(theme.panel),
                    search,
                    theme,
                ));
            }
            spans.push(Span::styled(
                format!("  {} lines", step.lines.len()),
                theme.dim_style(),
            ));
            Line::from(spans)
        }
        LogsRow::Line { .. } => {
            let text = view.row_text(row);
            let mut spans = vec![Span::styled("    ", theme.base())];
            spans.extend(super::highlight_spans(text, theme.base(), search, theme));
            Line::from(spans)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::standard_fixture;

    fn log_event(text: &str) -> AppEvent {
        AppEvent::CiLog {
            text: text.to_owned(),
            next_offset: text.len() as u64,
            done: true,
        }
    }

    fn build_app() -> App {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(log_event(
            "lint\tUNKNOWN STEP\t2026-06-20T00:00:01Z ##[group]Build\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:02Z compiling…\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:03Z ##[endgroup]\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:04Z ##[group]Test\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:05Z running tests\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:06Z ok\n",
        ));
        app
    }

    #[test]
    fn renders_folded_steps_by_default() {
        let mut app = build_app();
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_waiting_state_when_empty() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn parses_sections_by_group_marker() {
        let app = build_app();
        let view = app.logs().expect("logs view");
        assert_eq!(view.steps.len(), 2);
        assert_eq!(view.steps[0].name, "Build");
        assert_eq!(view.steps[1].name, "Test");
        assert!(view.steps.iter().all(|s| s.folded));
    }
}
