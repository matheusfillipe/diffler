//! Log screen: `oid7 [refs] subject` rows, neogit's `l l` view.

use diffler_core::vcs::LogEntry;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::Hint;
use crate::ui::{cursor_line, hint_line, status_bar};

/// Hint entries, rendered against the live keymap so remaps show.
const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Open], "open commit"),
    Hint::Leaf(&[Action::MoveDown, Action::MoveUp], "move"),
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

    if let Some(log) = app.log.as_mut() {
        log.viewport = body.height;
        let height = body.height.max(1) as usize;
        if log.cursor < log.scroll {
            log.scroll = log.cursor;
        }
        if log.cursor >= log.scroll + height {
            log.scroll = log.cursor + 1 - height;
        }
        let lines: Vec<Line<'static>> = log
            .entries
            .iter()
            .enumerate()
            .skip(log.scroll)
            .take(height)
            .map(|(index, entry)| {
                let line = entry_line(&app.theme, entry);
                if index == log.cursor {
                    cursor_line(line, &app.theme, body.width)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), body);
    }

    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

fn entry_line(theme: &Theme, entry: &LogEntry) -> Line<'static> {
    let mut spans = vec![Span::styled(format!(" {} ", entry.oid7), theme.dim_style())];
    if !entry.refs.is_empty() {
        spans.push(Span::styled(
            format!("[{}] ", entry.refs.join(" ")),
            Style::new().fg(theme.accent).bg(theme.bg),
        ));
    }
    spans.push(Span::styled(entry.subject.clone(), theme.base()));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::test_support::{key, standard_fixture};

    fn render(app: &mut App) -> Terminal<TestBackend> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, app))
            .expect("draw");
        terminal
    }

    #[test]
    fn log_screen_renders_rows_with_refs_and_cursor() {
        let fixture = standard_fixture();
        fixture.write("notes.txt", "alpha\nbeta\n");
        fixture.commit_all("add beta note");
        fixture.branch("feat/topic");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        app.handle(key('j'));
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        assert!(content.contains("feat/topic"), "refs decorate: {content}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn log_scrolls_to_keep_the_cursor_visible() {
        let fixture = standard_fixture();
        for index in 0..30 {
            fixture.write("notes.txt", &format!("rev {index}\n"));
            fixture.commit_all(&format!("commit number {index}"));
        }
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        app.handle(key('G'));
        let backend = TestBackend::new(120, 10);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("draw");
        let content = terminal.backend().to_string();
        assert!(
            content.contains("initial commit"),
            "view follows the cursor to the oldest commit: {content}"
        );
    }
}
