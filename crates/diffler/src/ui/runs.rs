//! The CI runs start page: a hint line, the list of recent runs for the repo's
//! provider, and the shared status bar. Selecting a run opens its graph.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::keymap::Action;
use crate::ui::{Hint, hint_line};

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::MoveDown, Action::MoveUp], "move"),
    Hint::Leaf(&[Action::Open], "open graph"),
    Hint::Leaf(&[Action::Search], "search"),
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
    draw_list(frame, app, body);
    frame.render_widget(Paragraph::new(super::status_bar(app, bar.width)), bar);
}

fn draw_list(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.runs.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("  no runs yet…", app.theme.dim_style())),
            area,
        );
        return;
    }
    let height = area.height.max(1) as usize;
    let scroll = app.runs_selected().saturating_sub(height - 1);
    let rows: Vec<Line<'static>> = app
        .runs
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(i, run)| {
            let selected = i == app.runs_selected();
            let glyph = run.status.glyph();
            let color = super::ci_status_color(&app.theme, run.status);
            let short = run.commit.chars().take(7).collect::<String>();
            let marker = if selected { "▌ " } else { "  " };
            let name_style = if selected {
                Style::new()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(app.theme.accent)
            };
            let mut spans = vec![
                Span::styled(marker, Style::new().fg(app.theme.warn_fg)),
                Span::styled(format!("{glyph} "), Style::new().fg(color)),
                Span::styled(format!("{:<16}", truncate(&run.name, 16)), name_style),
                Span::styled(
                    format!("  {:<32}", truncate(&run.title, 32)),
                    Style::new().fg(app.theme.fg),
                ),
                Span::styled(
                    format!("  {:<14}", truncate(&run.branch, 14)),
                    Style::new().fg(app.theme.purple),
                ),
                Span::styled(format!("  {short}"), Style::new().fg(app.theme.warn_fg)),
            ];
            if let Some(created) = run.created {
                let age = super::relative_time(app.now_unix, created.unix_timestamp());
                let used: usize = spans.iter().map(ratatui::text::Span::width).sum();
                if used + age.chars().count() + 1 < area.width as usize {
                    let gap = area.width as usize - used - age.chars().count() - 1;
                    spans.push(Span::raw(" ".repeat(gap)));
                    spans.push(Span::styled(age, app.theme.dim_style()));
                }
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{CiRun, JobStatus, RunId};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    fn run(name: &str, branch: &str, sha: &str, status: JobStatus) -> CiRun {
        CiRun {
            id: RunId(name.to_owned()),
            name: name.to_owned(),
            title: "fix the thing".to_owned(),
            branch: branch.to_owned(),
            commit: sha.to_owned(),
            author: String::new(),
            created: None,
            status,
            url: None,
            remote: None,
        }
    }

    #[test]
    fn renders_the_runs_list_with_a_selection() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.runs = vec![
            run("CI", "main", "abc1234def", JobStatus::Ok),
            run("Release", "feature/x", "9988776655", JobStatus::Failed),
            run("Nightly", "main", "0011223344", JobStatus::Running),
        ];
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_empty_runs() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
