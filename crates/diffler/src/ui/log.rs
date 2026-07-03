//! Log screen: `oid7 [refs] subject` rows, neogit's `l l` view.

use diffler_core::vcs::LogEntry;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::Hint;
use crate::ui::{commit_meta_spans, cursor_line, hint_line, status_bar};

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

    let now = app.now_unix;
    let search = app.search.as_ref();
    if let Some(log) = app.log.as_mut() {
        log.viewport = body.height;
        log.body = body;
        let height = body.height.max(1) as usize;
        log.scroll = super::scroll_to_cursor(log.cursor, log.scroll, height);
        let selection = log.selection();
        let selected =
            |index: usize| selection.is_some_and(|(start, end)| index >= start && index <= end);
        let lines: Vec<Line<'static>> = log
            .entries
            .iter()
            .enumerate()
            .skip(log.scroll)
            .take(height)
            .map(|(index, entry)| {
                let ranges = search.map(|s| s.ranges_for(index)).unwrap_or_default();
                // the cursor and every row in the visual range get the marker
                // bar, bold text, and cursor-line tint, like the runs list
                if index == log.cursor || selected(index) {
                    let mut line = entry_line(
                        &app.theme,
                        entry,
                        now,
                        body.width.saturating_sub(1),
                        &ranges,
                    );
                    for span in &mut line.spans {
                        span.style = span.style.add_modifier(Modifier::BOLD);
                    }
                    line.spans
                        .insert(0, Span::styled("▌", Style::new().fg(app.theme.warn_fg)));
                    cursor_line(line, &app.theme, body.width)
                } else {
                    entry_line(&app.theme, entry, now, body.width, &ranges)
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

fn entry_line(
    theme: &Theme,
    entry: &LogEntry,
    now: i64,
    width: u16,
    search: &[(std::ops::Range<usize>, bool)],
) -> Line<'static> {
    let mut spans = vec![Span::styled(format!(" {} ", entry.oid7), theme.dim_style())];
    if !entry.refs.is_empty() {
        spans.push(Span::styled(
            format!("[{}] ", entry.refs.join(" ")),
            Style::new().fg(theme.accent).bg(theme.bg),
        ));
    }
    spans.extend(super::highlight_spans(
        &entry.subject,
        theme.base(),
        search,
        theme,
    ));
    let used: usize = spans.iter().map(Span::width).sum();
    spans.extend(commit_meta_spans(
        theme,
        &entry.author,
        entry.time_unix,
        now,
        used,
        width as usize,
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::test_support::{key, mouse_click, standard_fixture};

    fn render(app: &mut App) -> Terminal<TestBackend> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, app))
            .expect("draw");
        terminal
    }

    fn newest_commit(app: &App) -> i64 {
        app.log
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .map(|e| e.time_unix)
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn clicking_a_log_row_selects_it() {
        let fixture = standard_fixture();
        fixture.write("notes.txt", "a\n");
        fixture.commit_all("second");
        fixture.write("notes.txt", "a\nb\n");
        fixture.commit_all("third");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        render(&mut app);
        assert!(app.log.as_ref().unwrap().entries.len() >= 2);
        let (body, scroll) = {
            let log = app.log.as_ref().unwrap();
            (log.body, log.scroll as u16)
        };
        app.handle(mouse_click(body.x + 1, body.y + 1 - scroll));
        assert_eq!(app.log.as_ref().unwrap().cursor, 1);
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
        app.now_unix = newest_commit(&app) + 3661;
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        assert!(content.contains("feat/topic"), "refs decorate: {content}");
        assert!(content.contains("1h"), "commit age renders: {content}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn log_visual_selection_highlights_the_range() {
        let fixture = standard_fixture();
        fixture.write("notes.txt", "alpha\nbeta\n");
        fixture.commit_all("add beta note");
        fixture.write("notes.txt", "alpha\nbeta\ngamma\n");
        fixture.commit_all("add gamma note");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        // V at the top commit, then j extends the selection over two rows
        app.handle(key('V'));
        app.handle(key('j'));
        assert_eq!(app.log.as_ref().unwrap().selection(), Some((0, 1)));
        app.now_unix = newest_commit(&app) + 3661;
        let terminal = render(&mut app);
        // the two-row selection tints more cells than a bare cursor row does
        let bg = format!("{:?}", app.theme.cursor_line);
        let selected = format!("{:?}", terminal.backend().buffer())
            .matches(&bg)
            .count();
        app.log.as_mut().unwrap().visual_anchor = None;
        let unselected = format!("{:?}", render(&mut app).backend().buffer())
            .matches(&bg)
            .count();
        assert!(
            selected > unselected,
            "selection must paint extra rows: {selected} vs {unselected}"
        );
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
