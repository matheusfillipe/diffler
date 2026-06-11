//! Placeholder UI. Replaced by the real screens in M1.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

const BG: Color = Color::Rgb(0x0d, 0x11, 0x17);
const FG: Color = Color::Rgb(0xe6, 0xed, 0xf3);
const DIM: Color = Color::Rgb(0x8b, 0x94, 0x9e);
const BORDER: Color = Color::Rgb(0x30, 0x36, 0x3d);

pub fn draw(frame: &mut Frame<'_>, repo: Option<&Path>) {
    let area = frame.area();
    let repo_line = repo.map_or_else(
        || "not inside a git repository".to_owned(),
        |p| p.display().to_string(),
    );
    let body = Paragraph::new(format!(
        "work in progress\n\n{repo_line}\n\npress q to quit"
    ))
    .alignment(Alignment::Center)
    .style(Style::default().fg(DIM).bg(BG))
    .block(
        Block::default()
            .title(" diffler ")
            .title_style(Style::default().fg(FG))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(BG)),
    );
    frame.render_widget(body, area);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn placeholder_renders() {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw(frame, Some(Path::new("/tmp/repo"))))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
