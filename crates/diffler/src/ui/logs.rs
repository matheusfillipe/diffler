//! The job log view: a hint line, the (scrollable) accumulated log text, and the
//! shared status bar. The text grows as the provider is polled by offset.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, body, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(" jk scroll · q back", app.theme.dim_style())),
        hint,
    );
    let text = if app.log_text().is_empty() {
        "  waiting for logs…".to_owned()
    } else {
        app.log_text().to_owned()
    };
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::new().fg(app.theme.fg))
            .scroll((app.log_scroll(), 0)),
        body,
    );
    frame.render_widget(Paragraph::new(super::status_bar(app, bar.width)), bar);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::standard_fixture;

    #[test]
    fn renders_accumulated_log_text() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(AppEvent::CiLog {
            text: "compiling…\nrunning tests\nok\n".to_owned(),
            next_offset: 27,
            done: true,
        });
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
}
