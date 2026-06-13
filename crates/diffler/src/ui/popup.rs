//! Transient popup framework, neogit-style: an action popup rendered as a
//! bottom split, plus confirm, input, and pick-one list modals.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::theme::Theme;

/// Neogit-style action popup: a titled bottom panel listing
/// `key → action` entries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Popup {
    pub title: String,
    /// `(key label, description)` pairs.
    pub entries: Vec<(String, String)>,
}

impl Popup {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let area = frame.area();
        let lines = self.lines(theme);
        // +1 for the top border carrying the title
        let height = (lines.len() as u16 + 1).min(area.height);
        let popup_area = Rect {
            x: area.x,
            y: area.y + area.height - height,
            width: area.width,
            height,
        };
        frame.render_widget(Clear, popup_area);
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(theme.border).bg(theme.panel))
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::new()
                    .fg(theme.accent)
                    .bg(theme.panel)
                    .add_modifier(Modifier::BOLD),
            ));
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(block),
            popup_area,
        );
    }

    fn lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let key_style = Style::new()
            .fg(theme.purple)
            .bg(theme.panel)
            .add_modifier(Modifier::BOLD);
        let dim = Style::new().fg(theme.dim).bg(theme.panel);
        let fg = Style::new().fg(theme.fg).bg(theme.panel);

        let mut lines = vec![Line::styled("Actions", dim)];
        for (key, description) in &self.entries {
            lines.push(Line::from(vec![
                Span::styled(format!(" {key}"), key_style),
                Span::styled(format!("  {description}"), fg),
            ]));
        }
        lines
    }
}

/// Yes/no question rendered as a small centered modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmDialog {
    pub message: String,
}

impl ConfirmDialog {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let width = (self.message.len() as u16 + 4).clamp(24, frame.area().width);
        let area = centered(frame.area(), width, 4);
        frame.render_widget(Clear, area);
        let block = bordered_block(theme, " Confirm ");
        let body = vec![
            Line::styled(
                self.message.clone(),
                Style::new().fg(theme.fg).bg(theme.panel),
            ),
            Line::styled(
                "y confirm   n cancel",
                Style::new().fg(theme.dim).bg(theme.panel),
            ),
        ];
        frame.render_widget(
            Paragraph::new(body)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(block),
            area,
        );
    }
}

/// Multi-line text input modal with a visible cursor cell. The buffer may
/// hold newlines; the modal grows with it up to a cap, then shows the tail
/// (the cursor lives near the end while typing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputModal {
    pub title: String,
    pub buffer: String,
    /// Cursor position as a character index into `buffer`.
    pub cursor: usize,
}

/// Buffer lines visible at once before the modal stops growing.
const INPUT_MAX_LINES: usize = 8;

impl InputModal {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let mut lines = self.input_lines(theme);
        let overflow = lines.len().saturating_sub(INPUT_MAX_LINES);
        lines.drain(..overflow);
        lines.push(Line::styled(
            "enter submit  a-enter newline  esc cancel",
            Style::new().fg(theme.dim).bg(theme.panel),
        ));
        // +2 for the borders
        let height = lines.len() as u16 + 2;
        let area = centered(frame.area(), frame.area().width.min(60), height);
        frame.render_widget(Clear, area);
        let block = bordered_block(theme, &format!(" {} ", self.title));
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(block),
            area,
        );
    }

    fn input_lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let fg = Style::new().fg(theme.fg).bg(theme.panel);
        let cursor_cell = Style::new().fg(theme.bg).bg(theme.accent);
        let mut lines = Vec::new();
        // char offset of the current line's start within the buffer; the
        // cursor at a line's end (on the newline itself) renders as a
        // trailing placeholder cell
        let mut offset = 0usize;
        for text in self.buffer.split('\n') {
            let len = text.chars().count();
            if (offset..=offset + len).contains(&self.cursor) {
                let column = self.cursor - offset;
                let before: String = text.chars().take(column).collect();
                let at: String = text
                    .chars()
                    .nth(column)
                    .map_or_else(|| " ".to_owned(), |c| c.to_string());
                let after: String = text.chars().skip(column + 1).collect();
                lines.push(Line::from(vec![
                    Span::styled(before, fg),
                    Span::styled(at, cursor_cell),
                    Span::styled(after, fg),
                ]));
            } else {
                lines.push(Line::styled(text.to_owned(), fg));
            }
            offset += len + 1;
        }
        lines
    }
}

/// Centered pick-one list (branches) with a j/k cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListModal {
    pub title: String,
    pub items: Vec<String>,
    pub cursor: usize,
}

impl ListModal {
    pub fn render(&self, frame: &mut Frame<'_>, theme: &Theme) {
        let hint = "j/k move  enter select  esc back";
        let width = self
            .items
            .iter()
            .map(|item| item.chars().count())
            .chain([hint.len(), self.title.chars().count() + 2])
            .max()
            .unwrap_or(0) as u16
            + 4;
        // +3: borders and the hint line
        let height = self.items.len() as u16 + 3;
        let area = centered(frame.area(), width.min(frame.area().width), height);
        frame.render_widget(Clear, area);
        let mut lines: Vec<Line<'static>> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let style = if index == self.cursor {
                    Style::new().fg(theme.fg).bg(theme.cursor_line)
                } else {
                    Style::new().fg(theme.fg).bg(theme.panel)
                };
                Line::styled(format!(" {item} "), style)
            })
            .collect();
        lines.push(Line::styled(
            hint,
            Style::new().fg(theme.dim).bg(theme.panel),
        ));
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::new().fg(theme.fg).bg(theme.panel))
                .block(bordered_block(theme, &format!(" {} ", self.title))),
            area,
        );
    }
}

fn bordered_block(theme: &Theme, title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border).bg(theme.panel))
        .title(Span::styled(
            title.to_owned(),
            Style::new()
                .fg(theme.accent)
                .bg(theme.panel)
                .add_modifier(Modifier::BOLD),
        ))
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Block;

    use super::*;

    /// Render a widget over a themed background so the split/overlay
    /// boundaries are visible in the snapshot.
    fn render(draw: impl Fn(&mut Frame<'_>, &Theme)) -> Terminal<TestBackend> {
        let theme = Theme::github_dark();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(Block::new().style(theme.base()), frame.area());
                frame.render_widget(
                    ratatui::text::Text::from("status screen content behind the popup"),
                    frame.area(),
                );
                draw(frame, &theme);
            })
            .expect("draw");
        terminal
    }

    #[test]
    fn popup_renders_as_bottom_split() {
        let popup = Popup {
            title: "Branch".to_owned(),
            entries: vec![
                ("c".to_owned(), "create and checkout".to_owned()),
                ("n".to_owned(), "create".to_owned()),
                ("D".to_owned(), "delete".to_owned()),
            ],
        };
        let terminal = render(|frame, theme| popup.render(frame, theme));
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn confirm_dialog_renders_centered() {
        let dialog = ConfirmDialog {
            message: "Discard changes to src/lib.rs?".to_owned(),
        };
        let terminal = render(|frame, theme| dialog.render(frame, theme));
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn input_modal_renders_with_cursor() {
        let modal = InputModal {
            title: "New branch".to_owned(),
            buffer: "feat/m1".to_owned(),
            cursor: 7,
        };
        let terminal = render(|frame, theme| modal.render(frame, theme));
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn input_modal_renders_a_two_line_buffer() {
        let modal = InputModal {
            title: "Comment".to_owned(),
            buffer: "first line\nsecond".to_owned(),
            cursor: 17,
        };
        let terminal = render(|frame, theme| modal.render(frame, theme));
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn input_modal_overflow_shows_the_last_lines() {
        let buffer = (1..=12)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let modal = InputModal {
            title: "Comment".to_owned(),
            cursor: buffer.chars().count(),
            buffer,
        };
        let terminal = render(|frame, theme| modal.render(frame, theme));
        let content = terminal.backend().to_string();
        assert!(content.contains("line 12"), "tail stays visible: {content}");
        assert!(content.contains("line 5 "), "8 lines fit: {content}");
        assert!(
            !content.contains("line 4 "),
            "older lines scroll away: {content}"
        );
    }
}
