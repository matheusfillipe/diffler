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
    Hint::Leaf(&[Action::Search], "search"),
    Hint::Leaf(&[Action::CopyFileFeedback], "yank"),
    Hint::Leaf(&[Action::Help], "help"),
];

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, header, body, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(hint_line(app, HINTS)), hint);
    let mut provenance = super::graph::run_header(app, &app.theme);
    if let Some(job) = app.open_job_name() {
        provenance.push_span(Span::styled(
            format!("  ▸ {job}"),
            Style::new().fg(app.theme.accent),
        ));
    }
    frame.render_widget(Paragraph::new(provenance), header);

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
            if let Some(status) = step.status {
                spans.push(Span::styled(
                    format!("{} ", status.glyph()),
                    Style::new().fg(super::ci_status_color(theme, status)),
                ));
            }
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
            if let Some(secs) = step.duration_secs {
                spans.push(Span::styled(
                    format!("  {}", fmt_duration(secs)),
                    theme.dim_style(),
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

/// A step's run time as `13s` or `1m03s`.
fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
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
            steps: Vec::new(),
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

    #[test]
    fn renders_real_steps_with_status_and_duration() {
        use crate::ci::{JobStatus, LogStepMeta, ts_sort_key};
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(AppEvent::CiLog {
            text: "lint\tUNKNOWN STEP\t2026-06-20T00:00:00Z setting up\n\
                   lint\tUNKNOWN STEP\t2026-06-20T00:00:05Z running clippy\n"
                .to_owned(),
            steps: vec![
                LogStepMeta {
                    name: "Set up job".into(),
                    status: JobStatus::Ok,
                    start_key: ts_sort_key("2026-06-20T00:00:00Z"),
                    duration_secs: Some(2),
                },
                LogStepMeta {
                    name: "Run cargo clippy".into(),
                    status: JobStatus::Failed,
                    start_key: ts_sort_key("2026-06-20T00:00:05Z"),
                    duration_secs: Some(73),
                },
            ],
            next_offset: 0,
            done: true,
        });
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
