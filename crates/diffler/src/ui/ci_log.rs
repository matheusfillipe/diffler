//! A CI job's log folded into its steps.

use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::app::ci_log::{CiLogRow, CiLogView, LogLineKind, workflow_command};
use crate::app::rowsel::RowSelect;
use crate::ci::fmt_duration;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::{Hint, cursor_line, status_bar};

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::ToggleFold], "fold"),
    Hint::Leaf(&[Action::Search], "search"),
    Hint::Leaf(&[Action::Yank], "yank"),
    Hint::Leaf(&[Action::Help], "help"),
];

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (header, body, bar) = super::screen_chrome_with_header(frame, app, HINTS);
    let mut provenance = super::graph::run_header(app, &app.theme);
    if let Some(job) = app.open_job_name() {
        provenance.push_span(Span::styled(
            format!("  ▸ {job}"),
            Style::new().fg(app.theme.accent),
        ));
    }
    frame.render_widget(Paragraph::new(provenance), header);

    let search = app.search.as_ref();
    match app.ci_log.as_mut() {
        Some(view) if !view.steps.is_empty() => {
            view.viewport = body.height;
            view.body = body;
            let height = body.height.max(1) as usize;
            let rows = view.rows();
            view.scroll = super::scroll_to_cursor(view.cursor, view.scroll, height, rows.len());
            let selected = |i: usize| view.row_selected(i);
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
    view: &CiLogView,
    row: CiLogRow,
    search: &[(std::ops::Range<usize>, bool)],
) -> Line<'static> {
    match row {
        CiLogRow::Step(s) => {
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
        CiLogRow::Line { step, line } => {
            let text = view.line_text(row);
            let kind = view
                .steps
                .get(step)
                .and_then(|step| step.kinds.get(line))
                .copied()
                .unwrap_or(LogLineKind::Plain);
            let shown = match (kind, workflow_command(text)) {
                (LogLineKind::GroupEdge, Some(("endgroup", _))) => "",
                (_, Some((_, rest))) => rest,
                (_, None) => text,
            };
            let skipped = text.len() - shown.len();
            let search: Vec<_> = search
                .iter()
                .filter(|(range, _)| range.end > skipped)
                .map(|(range, current)| {
                    (
                        range.start.saturating_sub(skipped)..range.end - skipped,
                        *current,
                    )
                })
                .collect();
            let mut spans = vec![Span::styled("    ", theme.base())];
            spans.extend(super::highlight_spans(
                shown,
                line_style(theme, kind),
                &search,
                theme,
            ));
            Line::from(spans)
        }
    }
}

fn line_style(theme: &Theme, kind: LogLineKind) -> Style {
    match kind {
        LogLineKind::Plain => theme.base(),
        LogLineKind::Muted | LogLineKind::GroupEdge => theme.dim_style(),
        LogLineKind::Command | LogLineKind::Notice => theme.base().fg(theme.accent),
        LogLineKind::Error => theme.base().fg(theme.error_fg),
        LogLineKind::Warning => theme.base().fg(theme.warn_fg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    fn build_app() -> App {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let event = app.test_log_event(
            "lint\tUNKNOWN STEP\t2026-06-20T00:00:01Z ##[group]Build\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:02Z compiling…\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:03Z ##[endgroup]\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:04Z ##[group]Test\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:05Z running tests\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:06Z ok\n",
            Vec::new(),
        );
        app.handle(event);
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
    fn an_unfolded_step_hides_workflow_markers_and_dims_runner_setup() {
        use crate::ci::{JobStatus, LogStepMeta, ts_sort_key};
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let event = app.test_log_event(
            "2026-06-20T00:00:00Z ##[group]Run cargo test\n\
             2026-06-20T00:00:00Z cargo test\n\
             2026-06-20T00:00:00Z shell: /usr/bin/bash -e {0}\n\
             2026-06-20T00:00:00Z ##[endgroup]\n\
             2026-06-20T00:00:01Z test widgets ... FAILED\n\
             2026-06-20T00:00:02Z ##[error]Process completed with exit code 1.\n\
             2026-06-20T00:00:03Z [command]/usr/bin/git version\n",
            vec![LogStepMeta {
                name: "Run cargo test".into(),
                status: JobStatus::Failed,
                start_key: ts_sort_key("2026-06-20T00:00:00Z"),
                end_key: 0,
                duration_secs: Some(3),
            }],
        );
        app.handle(event);
        let view = app.ci_log.as_mut().expect("ci_log view");
        view.steps[0].folded = false;
        assert_eq!(
            view.steps[0].kinds,
            [
                LogLineKind::GroupEdge,
                LogLineKind::Muted,
                LogLineKind::Muted,
                LogLineKind::GroupEdge,
                LogLineKind::Plain,
                LogLineKind::Error,
                LogLineKind::Command,
            ]
        );
        let backend = TestBackend::new(60, 11);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let error_row = (0..buffer.area.height)
            .find(|&y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Process completed")
            })
            .expect("error line on screen");
        assert_eq!(buffer[(4, error_row)].fg, app.theme.error_fg);
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
        let view = app.ci_log().expect("ci_log view");
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
        let event = app.test_log_event(
            "lint\tUNKNOWN STEP\t2026-06-20T00:00:00Z setting up\n\
             lint\tUNKNOWN STEP\t2026-06-20T00:00:05Z running clippy\n",
            vec![
                LogStepMeta {
                    name: "Set up job".into(),
                    status: JobStatus::Ok,
                    start_key: ts_sort_key("2026-06-20T00:00:00Z"),
                    end_key: 0,
                    duration_secs: Some(2),
                },
                LogStepMeta {
                    name: "Run cargo clippy".into(),
                    status: JobStatus::Failed,
                    start_key: ts_sort_key("2026-06-20T00:00:05Z"),
                    end_key: 0,
                    duration_secs: Some(73),
                },
            ],
        );
        app.handle(event);
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
