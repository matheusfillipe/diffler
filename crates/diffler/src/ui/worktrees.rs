//! The repository's worktrees, one row each.

use diffler_core::vcs::Worktree;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::app::fuzzy::{FuzzyFocus, FuzzyList};
use crate::app::tabs::home_relative;
use crate::keymap::Action;
use crate::text::elide;
use crate::ui::Hint;

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Open], "open in a tab"),
    Hint::Leaf(&[Action::WorktreeCreate], "create"),
    Hint::Leaf(&[Action::WorktreeCheckout], "check out a branch"),
    Hint::Leaf(&[Action::WorktreeRemove], "remove"),
    Hint::Leaf(&[Action::Help], "help"),
];

/// Columns past this width get cut, so one long branch name or path never
/// pushes the rest off screen.
const LABEL_WIDTH: usize = 32;
const PATH_WIDTH: usize = 48;

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (body, bar) = super::screen_chrome(frame, app, HINTS);
    draw_list(frame, app, body);
    frame.render_widget(Paragraph::new(super::status_bar(app, bar.width)), bar);
}

pub(super) fn picker(
    worktrees: &[Worktree],
    list: &FuzzyList,
    remove: bool,
) -> super::popup::FuzzyModal {
    let (title, verb) = if remove {
        ("Remove worktree", " remove")
    } else {
        ("Open worktree", " open")
    };
    super::popup::FuzzyModal {
        title: title.to_owned(),
        query: list.query.clone(),
        cursor: list.cursor,
        typing: matches!(list.focus, FuzzyFocus::Input),
        items: list
            .matches
            .iter()
            .filter_map(|index| worktrees.get(*index))
            .map(|w| (w.label.clone(), home_relative(&w.path)))
            .collect(),
        selected: list.selected,
        footer: super::footer_for(list, "", verb),
    }
}

fn draw_list(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let worktrees = &app.status.worktrees;
    if worktrees.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "  no worktree to list, c to create one",
                theme.dim_style(),
            )),
            area,
        );
        return;
    }
    let height = area.height.max(1) as usize;
    let scroll = super::scroll_to_cursor(
        app.worktrees_cursor,
        app.worktrees_scroll,
        height,
        worktrees.len(),
    );
    let paths: Vec<String> = worktrees.iter().map(|w| home_relative(&w.path)).collect();
    let label_width = column_width(worktrees.iter().map(|w| w.label.as_str()), LABEL_WIDTH);
    let path_width = column_width(paths.iter().map(String::as_str), PATH_WIDTH);
    let rows: Vec<Line<'static>> = worktrees
        .iter()
        .zip(&paths)
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(index, (worktree, path))| {
            let ranges = app
                .search
                .as_ref()
                .map(|search| search.ranges_for(index))
                .unwrap_or_default();
            let (marker, marker_style) = if worktree.missing {
                ("✗ ", Style::new().fg(theme.error_fg))
            } else if worktree.current {
                ("* ", theme.dim_style())
            } else {
                ("  ", theme.dim_style())
            };
            let label_style = if worktree.missing {
                Style::new().fg(theme.error_fg)
            } else {
                super::status::branch_name_style(theme)
            };
            let label = fit(&worktree.label, label_width);
            let mut spans = vec![Span::raw("  "), Span::styled(marker, marker_style)];
            spans.extend(super::highlight_spans(&label, label_style, &ranges, theme));
            spans.push(Span::styled(
                format!("  {}  ", fit(path, path_width)),
                theme.dim_style(),
            ));
            if worktree.missing {
                spans.push(Span::styled(
                    "folder gone, x to forget it",
                    theme.dim_style(),
                ));
            } else {
                spans.push(Span::styled(
                    worktree.subject.clone(),
                    Style::new().fg(theme.fg),
                ));
                let used: usize = spans.iter().map(Span::width).sum();
                spans.extend(super::age_spans(
                    theme,
                    worktree.time_unix,
                    app.now_unix,
                    used,
                    area.width as usize,
                ));
            }
            let line = Line::from(spans);
            if index == app.worktrees_cursor {
                super::cursor_line(line, theme, area.width)
            } else {
                line
            }
        })
        .collect();
    app.worktrees_scroll = scroll;
    frame.render_widget(Paragraph::new(rows), area);
}

fn column_width<'a>(texts: impl Iterator<Item = &'a str>, cap: usize) -> usize {
    texts
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0)
        .min(cap)
}

/// `text` cut to `width` columns, then padded out to them.
fn fit(text: &str, width: usize) -> String {
    let cut = elide(text, width);
    let pad = width.saturating_sub(cut.width());
    format!("{cut}{}", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    fn worktree(label: &str, path: &str, current: bool, missing: bool) -> Worktree {
        Worktree {
            name: label.to_owned(),
            path: path.into(),
            label: label.to_owned(),
            branch: (!missing).then(|| label.to_owned()),
            subject: if missing {
                String::new()
            } else {
                format!("work on {label}")
            },
            time_unix: 1_000_000 - 120,
            current,
            main: current,
            missing,
        }
    }

    fn render(app: &mut App) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(100, 7)).expect("terminal");
        terminal.draw(|f| draw(f, app)).expect("draw");
        terminal
    }

    #[test]
    fn renders_each_worktree_with_its_branch_path_and_age() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.push_screen(crate::app::Screen::Worktrees);
        app.now_unix = 1_000_000;
        app.status.worktrees = vec![
            worktree("main", "/work/widgets", true, false),
            worktree(
                "agent/fix-avatars",
                "/work/widgets-fix-avatars",
                false,
                false,
            ),
            worktree("old-spike", "/work/widgets-old-spike", false, true),
        ];
        app.worktrees_cursor = 1;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn an_empty_list_names_the_key_that_creates_one() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.push_screen(crate::app::Screen::Worktrees);
        app.status.worktrees.clear();
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("c to create one"), "{content}");
    }

    #[test]
    fn fit_pads_short_text_and_cuts_long_text() {
        assert_eq!(fit("main", 6), "main  ");
        assert_eq!(fit("feature/long", 8), "feature…");
    }
}
