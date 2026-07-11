//! The pull-request list: open PRs of the repo's forge. Enter reviews the
//! selected PR in place (no checkout needed); `b` checks its branch out.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::keymap::Action;
use crate::ui::Hint;

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Open], "review"),
    Hint::Leaf(&[Action::BranchCheckout], "checkout"),
    Hint::Leaf(&[Action::Search], "search"),
    Hint::Leaf(&[Action::Help], "help"),
];

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (body, bar) = super::screen_chrome(frame, app, HINTS);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::PullRequest;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    fn pr(number: u64, title: &str, author: &str, head_ref: &str, base_ref: &str) -> PullRequest {
        PullRequest {
            number,
            title: title.to_owned(),
            url: None,
            base_ref: base_ref.to_owned(),
            head_ref: head_ref.to_owned(),
            head_oid: "0".repeat(40),
            author: author.to_owned(),
        }
    }

    #[test]
    fn renders_the_pr_list_with_a_selection() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.prs = vec![
            pr(12, "Add widgets", "alice", "feat/widgets", "main"),
            pr(9, "Fix flaky test", "bob", "fix/flaky", "main"),
        ];
        app.prs_cursor = 1;
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn renders_no_open_pull_requests() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
