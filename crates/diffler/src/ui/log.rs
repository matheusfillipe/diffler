//! Log screen: `graph oid7 [refs] subject` rows, neogit's `l l` and `l b`
//! views.

use diffler_core::loggraph::{Cell, Lane};
use diffler_core::vcs::LogEntry;
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::app::file::FileHistory;
use crate::app::rowsel::RowSelect;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::Hint;
use crate::ui::{commit_meta_spans, cursor_line, status_bar};

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Open], "open commit"),
    Hint::Leaf(&[Action::VisualSelect], "select range"),
    Hint::Leaf(
        &[Action::NextSection, Action::PrevSection],
        "step branch tips",
    ),
    Hint::Leaf(&[Action::Help], "help"),
];

const HISTORY_HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Open], "open commit"),
    Hint::Leaf(&[Action::Blame], "open this version"),
    Hint::Leaf(&[Action::VisualSelect], "select range"),
    Hint::Leaf(&[Action::Help], "help"),
];

/// Lanes past this many draw as one `…`, so a repo with dozens of open
/// branches keeps room for the subjects.
const MAX_LANES: usize = 12;

struct RowCtx<'a> {
    theme: &'a Theme,
    now: i64,
    width: u16,
    head: Option<&'a str>,
    branch: Option<&'a str>,
    remotes: &'a [String],
    /// The widest graph on screen, so every subject starts in one column.
    lanes: usize,
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let history = app.log.as_ref().and_then(|log| log.file.as_ref());
    let (body, bar) = if let Some(file) = history {
        let header_line = history_header(file, &app.theme);
        let (header, body, bar) = super::screen_chrome_with_header(frame, app, HISTORY_HINTS);
        frame.render_widget(Paragraph::new(header_line), header);
        (body, bar)
    } else {
        super::screen_chrome(frame, app, HINTS)
    };

    let search = app.search.as_ref();
    if let Some(log) = app.log.as_mut() {
        log.viewport = body.height;
        log.body = body;
        let height = body.height.max(1) as usize;
        log.scroll = super::scroll_to_cursor(log.cursor, log.scroll, height, log.entries.len());
        let selection = log.selection();
        let selected =
            |index: usize| selection.is_some_and(|(start, end)| index >= start && index <= end);
        let ctx = RowCtx {
            theme: &app.theme,
            now: app.now_unix,
            width: body.width,
            head: log.head.as_deref(),
            branch: app.head.branch.as_deref(),
            remotes: &log.remotes,
            lanes: log
                .graph
                .iter()
                .skip(log.scroll)
                .take(height)
                .map(Vec::len)
                .max()
                .unwrap_or(0)
                .min(MAX_LANES + 1),
        };
        let lines: Vec<Line<'static>> = log
            .entries
            .iter()
            .enumerate()
            .skip(log.scroll)
            .take(height)
            .map(|(index, entry)| {
                let ranges = search.map(|s| s.ranges_for(index)).unwrap_or_default();
                let graph = log.graph.get(index).map_or(&[][..], Vec::as_slice);
                let old_path = log.file.as_ref().and_then(|file| {
                    let path = &file.versions.get(index)?.path;
                    (*path != file.path).then_some(path.as_str())
                });
                let line = entry_line(&ctx, entry, graph, old_path, &ranges);
                if index == log.cursor || selected(index) {
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

fn history_header(file: &FileHistory, theme: &Theme) -> Line<'static> {
    let count = file.versions.len();
    Line::from(vec![
        Span::styled(format!("  {}", file.path), Style::new().fg(theme.accent)),
        Span::styled(
            format!("  {count} version{}", if count == 1 { "" } else { "s" }),
            theme.dim_style(),
        ),
    ])
}

fn entry_line(
    ctx: &RowCtx<'_>,
    entry: &LogEntry,
    graph: &[Cell],
    version_old_path: Option<&str>,
    search: &[(std::ops::Range<usize>, bool)],
) -> Line<'static> {
    let theme = ctx.theme;
    let mut spans = vec![Span::raw(" ")];
    spans.extend(graph_spans(
        theme,
        graph,
        ctx.head == Some(entry.oid.as_str()),
    ));
    let drawn = graph.len().min(MAX_LANES + 1);
    spans.push(Span::raw("  ".repeat(ctx.lanes.saturating_sub(drawn))));
    spans.push(Span::styled(
        format!(" {} ", entry.short_id()),
        theme.dim_style(),
    ));
    spans.extend(ref_spans(ctx, entry));
    if let Some(path) = version_old_path {
        spans.push(Span::styled(
            format!("{path} "),
            Style::new().fg(theme.purple),
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
        ctx.now,
        used,
        ctx.width as usize,
    ));
    Line::from(spans)
}

fn lane_color(theme: &Theme, lane: Option<Lane>) -> Color {
    match lane {
        Some(Lane { head: true, .. }) => theme.accent,
        Some(Lane { id, .. }) => super::color::slot_color(theme, id + 1),
        None => theme.dim,
    }
}

/// The graph's cells, the checked-out commit's dot drawn hollow and bold.
fn graph_spans(theme: &Theme, graph: &[Cell], is_head: bool) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for cell in graph.iter().take(MAX_LANES) {
        let mut style = Style::new()
            .fg(lane_color(theme, cell.glyph_lane))
            .bg(theme.bg);
        let glyph = if cell.glyph == '●' && is_head {
            style = style.add_modifier(Modifier::BOLD);
            '◉'
        } else {
            cell.glyph
        };
        spans.push(Span::styled(glyph.to_string(), style));
        spans.push(Span::styled(
            cell.fill.to_string(),
            Style::new()
                .fg(lane_color(theme, cell.fill_lane))
                .bg(theme.bg),
        ));
    }
    if graph.len() > MAX_LANES {
        spans.push(Span::styled("… ", theme.dim_style()));
    }
    spans
}

/// The branch checked out leads as `HEAD → name` in the accent, local
/// branches and tags follow in purple, remote branches dimmed.
fn ref_spans(ctx: &RowCtx<'_>, entry: &LogEntry) -> Vec<Span<'static>> {
    if entry.refs.is_empty() && ctx.head != Some(entry.oid.as_str()) {
        return Vec::new();
    }
    let theme = ctx.theme;
    let bracket = theme.dim_style();
    let mut names: Vec<Span<'static>> = Vec::new();
    if ctx.head == Some(entry.oid.as_str()) {
        let label = match ctx.branch.filter(|b| entry.refs.iter().any(|r| r == b)) {
            Some(branch) => format!("HEAD → {branch}"),
            None => "HEAD".to_owned(),
        };
        names.push(Span::styled(
            label,
            Style::new()
                .fg(theme.accent)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    for name in &entry.refs {
        if Some(name.as_str()) == ctx.branch && ctx.head == Some(entry.oid.as_str()) {
            continue;
        }
        let remote = ctx.remotes.iter().any(|remote| {
            name.strip_prefix(remote.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        });
        let style = if remote {
            theme.dim_style()
        } else {
            super::status::branch_name_style(theme)
        };
        names.push(Span::styled(name.clone(), style));
    }
    let mut spans = vec![Span::styled("[", bracket)];
    for (index, name) in names.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ", bracket));
        }
        spans.push(name);
    }
    spans.push(Span::styled("] ", bracket));
    spans
}

#[cfg(test)]
mod tests {
    use crate::app::rowsel::RowSelect;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::App;
    use crate::app::file::FileHistory;
    use crate::config::LoadedConfig;
    use crate::test_support::{jj_fixture, key, mouse_click, standard_fixture};
    use crate::ui::file::tests::version;

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
    fn a_file_history_lists_its_versions_under_the_file_and_names_an_old_path() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.show_file_history(FileHistory {
            path: "src/lib.rs".to_owned(),
            versions: vec![
                version("ccccccc", "tidy the answer", "src/lib.rs"),
                version("bbbbbbb", "move into src", "src/lib.rs"),
                version("aaaaaaa", "first answer", "lib.rs"),
            ],
        });
        app.now_unix = 86_400 * 3;
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
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
    fn a_jj_log_names_commits_by_change_id() {
        let fixture = jj_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        let entries = &mut app.log.as_mut().unwrap().entries;
        assert!(entries.iter().all(|e| e.change_id.is_some()));
        // jj picks random change ids, so we seat ones that snapshots can hold
        for (entry, id) in entries.iter_mut().zip(["kkmpptxz", "qpvuntsm"]) {
            entry.change_id = Some(id.to_owned());
            entry.time_unix = 1_700_000_000;
        }
        entries[0].subject = "(no description set)".to_owned();
        app.now_unix = 1_700_000_000 + 3661;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn every_branch_log_draws_the_fork_and_marks_head() {
        let fixture = standard_fixture();
        fixture.branch("feature");
        fixture.checkout("feature");
        fixture.write("feature.txt", "side\n");
        fixture.commit_all_at("work on the feature", 1_700_000_100);
        fixture.checkout("main");
        fixture.write("main.txt", "trunk\n");
        fixture.commit_all_at("work on main", 1_700_000_200);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('b'));
        app.now_unix = newest_commit(&app) + 3661;
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        assert!(content.contains("◉"), "HEAD's dot: {content}");
        assert!(content.contains("HEAD → main"), "{content}");
        assert!(content.contains("●─╯"), "the fork closes: {content}");
        insta::assert_snapshot!(terminal.backend());

        app.handle(key('q'));
        app.handle(key('l'));
        app.handle(key('l'));
        let current = app.log.as_ref().unwrap();
        assert!(
            current
                .entries
                .iter()
                .all(|e| e.subject != "work on the feature"),
            "l l walks HEAD's history alone"
        );
    }

    #[test]
    fn brackets_step_between_decorated_commits() {
        let fixture = standard_fixture();
        fixture.branch("old");
        fixture.write("notes.txt", "a\n");
        fixture.commit_all("second");
        fixture.write("notes.txt", "a\nb\n");
        fixture.commit_all("third");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.handle(key('l'));
        app.handle(key('l'));
        app.handle(key(']'));
        let log = app.log.as_ref().unwrap();
        assert_eq!(log.entries[log.cursor].refs, ["old"]);
        app.handle(key('['));
        assert_eq!(app.log.as_ref().unwrap().cursor, 0);
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
