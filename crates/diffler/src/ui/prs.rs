//! The pull-request list: open PRs of the repo's forge. Enter reviews the
//! selected PR in place (no checkout needed); `b` checks its branch out.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::keymap::Action;
use crate::ui::{Hint, hint_line};

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Open], "review"),
    Hint::Leaf(&[Action::BranchCheckout], "checkout"),
    Hint::Leaf(&[Action::Search], "search"),
    Hint::Leaf(&[Action::Help], "help"),
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
    if app.prs.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "  no open pull requests…",
                app.theme.dim_style(),
            )),
            area,
        );
        return;
    }
    let height = area.height.max(1) as usize;
    let scroll = app.prs_cursor.saturating_sub(height - 1);
    let rows: Vec<Line<'static>> = app
        .prs
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(i, pr)| {
            let selected = i == app.prs_cursor;
            let marker = if selected { "▌ " } else { "  " };
            let title_style = if selected {
                Style::new().fg(app.theme.fg).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(app.theme.fg)
            };
            let ranges = app
                .search
                .as_ref()
                .map(|s| s.ranges_for(i))
                .unwrap_or_default();
            let mut spans = vec![Span::styled(marker, Style::new().fg(app.theme.warn_fg))];
            spans.extend(super::highlight_spans(
                &format!("#{} {} {}", pr.number, pr.title, pr.author),
                title_style,
                &ranges,
                &app.theme,
            ));
            spans.push(Span::styled(
                format!("  {} → {}", pr.head_ref, pr.base_ref),
                Style::new().fg(app.theme.purple),
            ));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), area);
}
