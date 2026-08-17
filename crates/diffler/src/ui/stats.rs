//! The language breakdown: one row per language, GitHub's colours, ordered by
//! the column the reader picked.

use diffler_core::language;
use diffler_core::stats::{LanguageCount, RepoStats};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::Hint;

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::CycleSort], "sort"),
    Hint::Leaf(&[Action::Refresh], "rescan"),
    Hint::Leaf(&[Action::Help], "help"),
];

/// Cells the share bar occupies. Wide enough to separate a 2% language from a
/// 20% one, narrow enough to leave the numbers room.
const BAR_CELLS: usize = 18;

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (body, bar) = super::screen_chrome(frame, app, HINTS);
    draw_table(frame, app, body);
    frame.render_widget(Paragraph::new(super::status_bar(app, bar.width)), bar);
}

/// Rows the table spends on something other than a language: the column
/// header, the rule, the totals, and the line naming what the scan left out.
const CHROME_ROWS: usize = 4;

fn draw_table(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let Some(view) = app.stats.as_ref() else {
        return;
    };
    let Some(stats) = view.stats.as_ref() else {
        frame.render_widget(
            Paragraph::new(Line::styled("  counting…", theme.dim_style())),
            area,
        );
        return;
    };
    if stats.languages.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "  no files in a language diffler knows",
                theme.dim_style(),
            )),
            area,
        );
        return;
    }

    let rows = view.rows();
    let totals = stats.totals();
    let widest = rows.iter().map(|row| row.code).max().unwrap_or(1).max(1);
    let cursor = view.cursor.min(rows.len().saturating_sub(1));
    // a polyglot monorepo outruns the screen, so the language rows scroll under
    // the header while the totals stay pinned to the bottom
    let window = (area.height as usize).saturating_sub(CHROME_ROWS).max(1);
    let scroll = super::scroll_to_cursor(cursor, view.scroll, window, rows.len());
    let mut lines = vec![header_line(theme, view.sort)];
    lines.extend(
        rows.iter()
            .enumerate()
            .skip(scroll)
            .take(window)
            .map(|(index, row)| language_line(theme, row, widest, totals.code, index == cursor)),
    );
    lines.push(rule_line(theme, area.width));
    lines.push(totals_line(theme, &totals));
    lines.extend(left_out_line(theme, stats));
    frame.render_widget(Paragraph::new(lines), area);
    if let Some(view) = app.stats.as_mut() {
        view.scroll = scroll;
        view.viewport = u16::try_from(window).unwrap_or(u16::MAX);
    }
}

fn header_line(theme: &Theme, sort: crate::app::stats::StatsSort) -> Line<'static> {
    use crate::app::stats::StatsSort;
    let dim = theme.dim_style();
    let sorted = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
    let column = |label: &str, width: usize, active: bool| {
        Span::styled(
            format!("{label:>width$}"),
            if active { sorted } else { dim },
        )
    };
    Line::from(vec![
        Span::styled("  ", dim),
        Span::styled(
            format!("{:<16}", "Language"),
            if sort == StatsSort::Name { sorted } else { dim },
        ),
        column("Files", 6, sort == StatsSort::Files),
        column("Lines", 9, sort == StatsSort::Lines),
        column("Code", 9, sort == StatsSort::Code),
        column("Comments", 10, false),
        column("Blanks", 8, false),
    ])
}

fn language_line(
    theme: &Theme,
    row: &LanguageCount,
    widest: usize,
    total_code: usize,
    on_cursor: bool,
) -> Line<'static> {
    let bg = if on_cursor {
        theme.cursor_line
    } else {
        theme.bg
    };
    let dim = Style::new().fg(theme.dim).bg(bg);
    let plain = Style::new().fg(theme.fg).bg(bg);
    let hue = language_color(theme, row.color);
    // the bar is relative to the biggest language, and the smallest one keeps
    // a cell of its own
    let filled = (row.code * BAR_CELLS).div_ceil(widest).clamp(1, BAR_CELLS);
    let share = percent_tenths(row.code, total_code);
    Line::from(vec![
        Span::styled(
            if on_cursor { "▌" } else { " " },
            Style::new().fg(theme.accent).bg(bg),
        ),
        Span::styled("● ", Style::new().fg(hue).bg(bg)),
        Span::styled(format!("{:<16}", row.name), plain),
        Span::styled(format!("{:>6}", row.files), dim),
        Span::styled(format!("{:>9}", thousands(row.lines)), dim),
        Span::styled(format!("{:>9}", thousands(row.code)), plain),
        Span::styled(format!("{:>10}", thousands(row.comments)), dim),
        Span::styled(format!("{:>8}", thousands(row.blanks)), dim),
        Span::styled("  ", Style::new().bg(bg)),
        Span::styled("█".repeat(filled), Style::new().fg(hue).bg(bg)),
        Span::styled(
            "░".repeat(BAR_CELLS - filled),
            Style::new().fg(theme.border).bg(bg),
        ),
        Span::styled(format!("{share:>6}%"), dim),
    ])
}

/// `part` of `whole` as a percentage with one decimal, in integer arithmetic,
/// since a line count can outrun what a float holds exactly.
pub(super) fn percent_tenths(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "0.0".to_owned();
    }
    // rounded, so a table of shares adds up to about a hundred
    let tenths = (part * 1000 + whole / 2) / whole;
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn rule_line(theme: &Theme, width: u16) -> Line<'static> {
    Line::styled("─".repeat(width as usize), theme.dim_style())
}

fn totals_line(theme: &Theme, totals: &LanguageCount) -> Line<'static> {
    let dim = theme.dim_style();
    Line::from(vec![
        Span::styled("  ", dim),
        Span::styled(format!("{:<16}", "total"), dim),
        Span::styled(format!("{:>6}", totals.files), dim),
        Span::styled(format!("{:>9}", thousands(totals.lines)), dim),
        Span::styled(
            format!("{:>9}", thousands(totals.code)),
            Style::new().fg(theme.fg),
        ),
        Span::styled(format!("{:>10}", thousands(totals.comments)), dim),
        Span::styled(format!("{:>8}", thousands(totals.blanks)), dim),
    ])
}

/// What the scan did not count, so a reader can tell a missing language from a
/// deliberate omission.
fn left_out_line(theme: &Theme, stats: &RepoStats) -> Option<Line<'static>> {
    let mut parts = Vec::new();
    if stats.generated_files > 0 {
        parts.push(format!("{} generated", stats.generated_files));
    }
    if stats.unknown_files > 0 {
        parts.push(format!(
            "{} in no language diffler names",
            stats.unknown_files
        ));
    }
    if stats.skipped_files > 0 {
        parts.push(format!("{} binary or oversized", stats.skipped_files));
    }
    if parts.is_empty() {
        return None;
    }
    Some(Line::styled(
        format!("  left out: {}", parts.join(" · ")),
        theme.dim_style(),
    ))
}

/// Linguist's hue, lifted until it reads on this theme's background.
pub(super) fn language_color(theme: &Theme, color: language::Rgb) -> Color {
    let (r, g, b) = language::readable_on(color, rgb_of(theme.bg));
    Color::Rgb(r, g, b)
}

pub(super) fn rgb_of(color: Color) -> language::Rgb {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        // every bundled theme is truecolor; a terminal-palette colour has no
        // channels to compare, so treat it as the dark end
        _ => (0, 0, 0),
    }
}

/// Split `cells` between `shares` in proportion, by largest remainder, so the
/// pieces add up to exactly `cells` and no non-zero share rounds away to
/// nothing. A stacked bar is only honest if it fills its own width.
pub(super) fn allocate(shares: &[usize], cells: usize) -> Vec<usize> {
    let total: usize = shares.iter().sum();
    let counted = shares.iter().filter(|share| **share > 0).count();
    if total == 0 || cells == 0 || counted == 0 {
        return vec![0; shares.len()];
    }
    // every language that changed anything keeps a cell, so the rest of the
    // bar is what is left to share out
    if counted >= cells {
        return shares
            .iter()
            .scan(cells, |left, share| {
                let take = usize::from(*share > 0 && *left > 0);
                *left -= take;
                Some(take)
            })
            .collect();
    }
    let spare = cells - counted;
    let mut out: Vec<usize> = shares
        .iter()
        .map(|share| usize::from(*share > 0) + share * spare / total)
        .collect();
    let mut remainders: Vec<(usize, usize)> = shares
        .iter()
        .enumerate()
        .filter(|(_, share)| **share > 0)
        .map(|(index, share)| (index, share * spare % total))
        .collect();
    remainders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut short = cells - out.iter().sum::<usize>();
    for (index, _) in remainders {
        if short == 0 {
            break;
        }
        if let Some(cell) = out.get_mut(index) {
            *cell += 1;
            short -= 1;
        }
    }
    out
}

/// `49314` reads as `49,314`: the columns are for comparing magnitudes.
pub(super) fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, key, render};

    /// A repo in four languages, with a lockfile and a binary to leave out.
    fn polyglot() -> Fixture {
        let fixture = Fixture::new();
        fixture.write(
            "src/main.rs",
            "//! What this is.\n\nfn main() {\n    // why\n    println!(\"hi\");\n}\n",
        );
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n");
        fixture.write(
            "api/serve.py",
            "\"\"\"The server.\"\"\"\n\nimport os\n\nprint(os)\n",
        );
        fixture.write(
            "deploy/main.tf",
            "# infra\nresource \"null_resource\" \"a\" {}\n",
        );
        fixture.write("README.md", "# title\n\nprose\n");
        fixture.write("package-lock.json", "{\n  \"lockfileVersion\": 3\n}\n");
        fixture.write("logo.png", "\u{0}PNG binary");
        fixture.commit_all("base");
        fixture
    }

    /// Run the scan the worker would run, and hand the app its answer.
    fn count(app: &mut App) {
        let request = app.pending_stats.take().expect("a queued scan");
        let paths = app.review.vcs.tracked_files().expect("tracked files");
        let stats =
            diffler_core::stats::scan(&app.review.repo_root, &paths, &app.config.classify.rules());
        app.handle(AppEvent::RepoStats {
            stats: Box::new(stats),
            token: request.token,
        });
    }

    #[test]
    fn the_breakdown_renders_a_row_per_language() {
        let fixture = polyglot();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('L'));
        count(&mut app);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// A polyglot monorepo has more languages than a terminal has rows; the
    /// cursor has to stay on screen the way it does on every other list.
    #[test]
    fn the_table_scrolls_to_keep_the_cursor_in_view() {
        let fixture = polyglot();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('L'));
        let token = app.pending_stats.take().expect("a queued scan").token;
        let languages = LANGUAGES
            .iter()
            .enumerate()
            .map(|(index, name)| diffler_core::stats::LanguageCount {
                name,
                color: (0x80, 0x80, 0x80),
                files: 1,
                lines: 100 - index,
                code: 100 - index,
                comments: 0,
                blanks: 0,
                bytes: 0,
            })
            .collect();
        app.handle(AppEvent::RepoStats {
            stats: Box::new(diffler_core::stats::RepoStats {
                languages,
                unknown_files: 0,
                skipped_files: 0,
                generated_files: 0,
            }),
            token,
        });

        let top = render_at(&mut app, 12);
        assert!(top.contains("Rust"), "the first rows are up: {top}");
        assert!(!top.contains("Zig"), "the last row is off screen: {top}");

        app.handle(key('G'));
        let bottom = render_at(&mut app, 12);
        assert!(
            bottom.contains("Zig"),
            "the cursor row is on screen: {bottom}"
        );
        assert!(
            bottom.contains("total"),
            "the totals stay pinned under the rows: {bottom}"
        );
    }

    /// Draw into a short terminal, where the table has to scroll.
    fn render_at(app: &mut App, rows: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(100, rows);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, app))
            .expect("draw");
        terminal.backend().to_string()
    }

    const LANGUAGES: [&str; 14] = [
        "Rust",
        "Go",
        "Python",
        "TypeScript",
        "JavaScript",
        "Ruby",
        "Shell",
        "YAML",
        "TOML",
        "JSON",
        "Markdown",
        "HCL",
        "Nix",
        "Zig",
    ];

    #[test]
    fn the_screen_says_it_is_counting_until_the_scan_lands() {
        let fixture = polyglot();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('L'));
        let screen = render(&mut app).backend().to_string();
        assert!(screen.contains("counting"), "{screen}");
        count(&mut app);
        let screen = render(&mut app).backend().to_string();
        assert!(!screen.contains("counting"), "{screen}");
        assert!(screen.contains("Rust"), "{screen}");
    }

    #[test]
    fn allocate_fills_the_bar_exactly() {
        for shares in [
            vec![50, 30, 20],
            vec![97, 2, 1],
            vec![1, 1, 1, 1, 1, 1, 1],
            vec![100],
        ] {
            let cells = super::allocate(&shares, 16);
            assert_eq!(cells.iter().sum::<usize>(), 16, "{shares:?} -> {cells:?}");
            for (share, cell) in shares.iter().zip(&cells) {
                assert!(
                    *cell > 0,
                    "a language that changed keeps a cell: {shares:?}"
                );
                assert!(share > &0);
            }
        }
    }

    #[test]
    fn allocate_keeps_the_smallest_language_visible() {
        // 1% of the churn still owns a cell, and the rest divide what is left
        assert_eq!(super::allocate(&[980, 10, 10], 10), vec![8, 1, 1]);
    }

    #[test]
    fn allocate_degrades_when_there_are_more_languages_than_cells() {
        let cells = super::allocate(&[5, 4, 3, 2, 1], 3);
        assert_eq!(cells, vec![1, 1, 1, 0, 0], "the busiest three take the bar");
        assert_eq!(cells.iter().sum::<usize>(), 3);
    }

    #[test]
    fn allocate_of_nothing_is_nothing() {
        assert_eq!(super::allocate(&[], 8), Vec::<usize>::new());
        assert_eq!(super::allocate(&[0, 0], 8), vec![0, 0]);
        assert_eq!(super::allocate(&[1, 1], 0), vec![0, 0]);
    }

    #[test]
    fn thousands_groups_from_the_right() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(1_000), "1,000");
        assert_eq!(super::thousands(49_314), "49,314");
        assert_eq!(super::thousands(1_234_567), "1,234,567");
    }
}
