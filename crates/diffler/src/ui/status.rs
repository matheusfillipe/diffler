//! Status screen: hint line, head line, neogit-style sections with inline
//! diff expansion, recent commits, and the status bar.

use diffler_core::model::FileDiff;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use std::ops::Range;

use crate::app::{App, CI_TITLE, RECENT_TITLE, Row, Section};
use crate::config::FileLayout;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::transient::TransientKind;
use crate::ui::Hint;
use crate::ui::diff_render::render_hunk_lines;
use crate::ui::{
    commit_meta_spans, cursor_line, diffstat_spans, highlight_spans, hint_line, proportion_bar,
    status_bar, status_color,
};

/// Prefix-only hint entries: top-level keys and the transient prefixes,
/// rendered against the live keymap so remaps show. Sub-commands stay out of
/// the hint line — they appear in the which-key panel and the help popup.
const HINTS: &[Hint] = &[
    Hint::Prefix(TransientKind::Commit, "commit"),
    Hint::Prefix(TransientKind::Branch, "branch"),
    Hint::Prefix(TransientKind::Log, "log"),
    Hint::Prefix(TransientKind::Push, "push"),
    Hint::Prefix(TransientKind::Pull, "pull"),
    Hint::Prefix(TransientKind::Fetch, "fetch"),
    Hint::Prefix(TransientKind::Stash, "stash"),
    Hint::Leaf(&[Action::Stage], "stage"),
    Hint::Leaf(&[Action::Discard], "discard"),
    Hint::Leaf(&[Action::Help], "help"),
];

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, body_area, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    app.status.viewport = body_area.height;
    frame.render_widget(Paragraph::new(hint_line(app, HINTS)), hint);
    let (lines, scroll, line_rows) = body(app, body_area);
    app.status.body = body_area;
    app.status.scroll = scroll;
    app.status.line_rows = line_rows;
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body_area);
    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

/// Body lines, the vertical scroll keeping the cursor row in view, and a
/// per-rendered-line table of the `visible_rows` index each line belongs to
/// (`None` for headers/blanks) so mouse clicks map back to a row.
fn body(app: &App, area: Rect) -> (Vec<Line<'static>>, u16, Vec<Option<usize>>) {
    let mut lines = vec![head_line(app)];
    let (added, deleted) = Section::ALL
        .into_iter()
        .map(|section| section_diffstat(app, section))
        .fold((0, 0), |(a, d), (sa, sd)| (a + sa, d + sd));
    // omit the summary entirely when there is nothing to review
    if added != 0 || deleted != 0 {
        lines.push(changes_line(&app.theme, added, deleted));
    }
    lines.push(Line::default());
    let rows = app.visible_rows();
    let has_sections = rows
        .iter()
        .any(|row| matches!(row, Row::SectionHeader { .. }));
    if !has_sections {
        lines.push(centered_line(
            "nothing to review — working tree clean",
            app.theme.dim_style(),
            area.width,
        ));
        lines.push(Line::default());
    }

    let mut cursor_line_index = 0usize;
    // the preamble lines (head, optional changes summary, blanks, empty-state)
    // belong to no row
    let mut line_rows: Vec<Option<usize>> = vec![None; lines.len()];
    let mut index = 0;
    while let Some(row) = rows.get(index) {
        match row {
            // a hunk renders as one block: header + its diff lines, which all
            // follow contiguously in the flattened rows
            &Row::HunkHeader {
                section,
                file,
                hunk,
            } => {
                let Some(file_diff) = app.section_files(section).get(file) else {
                    index += 1;
                    continue;
                };
                let Some(hunk) = file_diff.hunks.get(hunk) else {
                    index += 1;
                    continue;
                };
                let span = 1 + hunk.lines.len();
                let selected = app
                    .status
                    .cursor
                    .checked_sub(index)
                    .filter(|offset| *offset < span);
                if let Some(offset) = selected {
                    cursor_line_index = lines.len() + offset;
                }
                let syntax = app
                    .status
                    .highlights
                    .get(&file_diff.path)
                    .map(|cached| (cached.old.as_slice(), cached.new.as_slice()));
                lines.extend(render_hunk_lines(
                    &app.theme, hunk, syntax, area.width, selected,
                ));
                line_rows.extend((0..span).map(|offset| Some(index + offset)));
                index += span;
            }
            row => {
                if index > 0
                    && matches!(
                        row,
                        Row::SectionHeader { .. } | Row::RecentHeader { .. } | Row::CiHeader { .. }
                    )
                {
                    lines.push(Line::default());
                    line_rows.push(None);
                }
                let on_cursor = index == app.status.cursor;
                if on_cursor {
                    cursor_line_index = lines.len();
                }
                let ranges = app
                    .search
                    .as_ref()
                    .map(|search| search.ranges_for(index))
                    .unwrap_or_default();
                lines.push(row_line(app, row, on_cursor, area.width, &ranges));
                line_rows.push(Some(index));
                index += 1;
            }
        }
    }

    let height = area.height.max(1) as usize;
    let scroll = cursor_line_index.saturating_sub(height - 1) as u16;
    (lines, scroll, line_rows)
}

fn centered_line(text: &str, style: Style, width: u16) -> Line<'static> {
    let pad = (width as usize).saturating_sub(text.chars().count()) / 2;
    Line::from(vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(text.to_owned(), style),
    ])
}

fn head_line(app: &App) -> Line<'static> {
    let theme = &app.theme;
    let mut spans = vec![Span::styled(" Head:     ", theme.dim_style())];
    match &app.head.branch {
        Some(branch) => spans.push(Span::styled(
            branch.clone(),
            Style::new().fg(theme.purple).bg(theme.bg),
        )),
        None => spans.push(Span::styled("(detached)", theme.dim_style())),
    }
    if app.head.oid7.is_empty() {
        spans.push(Span::styled(" (no commits)", theme.dim_style()));
    } else {
        spans.push(Span::styled(
            format!(" {}", app.head.oid7),
            theme.dim_style(),
        ));
        spans.push(Span::styled(format!(" {}", app.head.subject), theme.base()));
    }
    Line::from(spans)
}

/// Grand-total diffstat summary: ` Changes  +A -B  <bar>`, aligned under the
/// head line. The bar is a compact green:red proportion of added to deleted.
fn changes_line(theme: &Theme, added: usize, deleted: usize) -> Line<'static> {
    let mut spans = vec![Span::styled(" Changes  ", theme.dim_style())];
    spans.extend(diffstat_spans(theme, added, deleted, theme.bg));
    spans.push(Span::styled("  ", theme.base()));
    spans.extend(proportion_bar(theme, added, deleted, theme.bg));
    Line::from(spans)
}

fn row_line(
    app: &App,
    row: &Row,
    selected: bool,
    width: u16,
    search: &[(Range<usize>, bool)],
) -> Line<'static> {
    let theme = &app.theme;
    let spans = match row {
        Row::SectionHeader { section, count } => {
            let mut spans = header_spans(
                theme,
                section.title(),
                *count,
                app.is_folded(*section),
                search,
            );
            let (added, deleted) = section_diffstat(app, *section);
            spans.extend(diffstat_spans(theme, added, deleted, theme.bg));
            spans
        }
        Row::RecentHeader { count } => header_spans(
            theme,
            RECENT_TITLE,
            *count,
            app.status.recent_folded,
            search,
        ),
        Row::Dir {
            section,
            path,
            name,
            depth,
        } => dir_spans(
            theme,
            name,
            app.is_dir_folded(*section, path),
            *depth,
            search,
        ),
        Row::File {
            section,
            index,
            depth,
        } => {
            let file = app.section_files(*section).get(*index);
            file_spans(app, file, theme, *depth, search)
        }
        Row::Commit { index } => commit_spans(app, *index, theme, width, search),
        Row::CiHeader { count } => {
            header_spans(theme, CI_TITLE, *count, app.status.ci_folded, search)
        }
        Row::CiRun { index } => ci_run_spans(app, *index, theme, search),
        // hunk rows are rendered as blocks in `body`, never through here
        Row::HunkHeader { .. } | Row::DiffLine { .. } => Vec::new(),
    };
    let line = Line::from(spans);
    if selected {
        cursor_line(line, theme, width)
    } else {
        line
    }
}

fn header_spans(
    theme: &Theme,
    title: &str,
    count: usize,
    folded: bool,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let title_style = Style::new()
        .fg(theme.accent)
        .bg(theme.bg)
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(
        if folded { " ▸ " } else { " ▾ " },
        theme.dim_style(),
    )];
    spans.extend(highlight_spans(title, title_style, search, theme));
    spans.push(Span::styled(format!(" ({count})"), theme.dim_style()));
    spans
}

/// Indentation for a tree row at `depth` within a section: a base indent that
/// clears the header's fold arrow, plus two cells per level.
fn tree_indent(depth: usize) -> String {
    " ".repeat(5 + depth * 2)
}

/// A directory row: indent, fold arrow, the dim directory name.
fn dir_spans(
    theme: &Theme,
    name: &str,
    folded: bool,
    depth: usize,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(tree_indent(depth), theme.base()),
        Span::styled(
            if folded { "▸ " } else { "▾ " }.to_owned(),
            theme.dim_style(),
        ),
    ];
    spans.extend(highlight_spans(name, theme.base(), search, theme));
    spans
}

/// A file row. In the tree layout: indent, status glyph (colored), basename —
/// the directory rows above carry the path. In the flat magit list: a status
/// glyph plus the full repo-relative path, no indent. Both trail the viewed
/// check and the file's `+A -B` diffstat.
fn file_spans(
    app: &App,
    file: Option<&FileDiff>,
    theme: &Theme,
    depth: usize,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let Some(file) = file else {
        return Vec::new();
    };
    let glyph = file.status.glyph();
    let flat = app.config.ui.status_file_layout == FileLayout::List;
    let name = app.status_file_name(file);
    let indent = if flat {
        " ".to_owned()
    } else {
        tree_indent(depth)
    };
    let mut spans = vec![
        Span::styled(indent, theme.base()),
        Span::styled(
            format!("{glyph} "),
            Style::new()
                .fg(status_color(theme, file.status))
                .bg(theme.bg),
        ),
    ];
    spans.extend(highlight_spans(name, theme.base(), search, theme));
    if app.is_path_viewed(&file.path) {
        spans.push(Span::styled(" ✓", theme.dim_style()));
    }
    let (added, deleted) = file.diffstat();
    spans.extend(diffstat_spans(theme, added, deleted, theme.bg));
    spans
}

fn commit_spans(
    app: &App,
    index: usize,
    theme: &Theme,
    width: u16,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let Some(entry) = app.status.recent.get(index) else {
        return Vec::new();
    };
    let mut spans = vec![Span::styled(
        format!("     {} ", entry.oid7),
        Style::new().fg(theme.warn_fg),
    )];
    spans.extend(highlight_spans(
        &entry.subject,
        Style::new().fg(theme.fg),
        search,
        theme,
    ));
    let used: usize = spans.iter().map(Span::width).sum();
    spans.extend(commit_meta_spans(
        theme,
        &entry.author,
        entry.time_unix,
        app.now_unix,
        used,
        width as usize,
    ));
    spans
}

fn ci_run_spans(
    app: &App,
    index: usize,
    theme: &Theme,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let Some(run) = app.runs.get(index) else {
        return Vec::new();
    };
    let glyph = run.status.glyph();
    let color = super::ci_status_color(theme, run.status);
    let mut spans = vec![Span::styled(
        format!("     {glyph} "),
        Style::new().fg(color),
    )];
    spans.extend(highlight_spans(
        &run.name,
        Style::new().fg(theme.accent),
        search,
        theme,
    ));
    let pad = 14usize.saturating_sub(run.name.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    let short: String = run.commit.chars().take(7).collect();
    spans.push(Span::styled(
        format!("  {:<32}", elide(&run.title, 32)),
        Style::new().fg(theme.fg),
    ));
    spans.push(Span::styled(
        format!("  {:<18}", elide(&run.branch, 18)),
        Style::new().fg(theme.purple),
    ));
    spans.push(Span::styled(
        format!("  {short}"),
        Style::new().fg(theme.warn_fg),
    ));
    spans
}

/// Truncate to `max` graphemes with an ellipsis.
fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// Summed `(added, deleted)` over every file in a section.
fn section_diffstat(app: &App, section: Section) -> (usize, usize) {
    app.section_files(section)
        .iter()
        .map(FileDiff::diffstat)
        .fold((0, 0), |(a, d), (fa, fd)| (a + fa, d + fd))
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::{App, Row, Section};
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{
        Fixture, key, mouse_click, mouse_scroll, standard_fixture, two_hunk_fixture,
    };

    /// Render through the top-level draw so modal overlays and screen
    /// switching are covered too.
    fn render(app: &mut App) -> Terminal<TestBackend> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, app))
            .expect("draw");
        terminal
    }

    #[test]
    fn status_shows_inline_ci_section() {
        use diffler_ci::{CiRun, JobStatus, RunId};
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let run = |name: &str, branch: &str, sha: &str, status| CiRun {
            id: RunId(name.to_owned()),
            name: name.to_owned(),
            title: "ci run".to_owned(),
            branch: branch.to_owned(),
            commit: sha.to_owned(),
            author: String::new(),
            created: None,
            status,
            url: None,
        };
        app.runs = vec![
            run("CI", "main", "abc1234def", JobStatus::Failed),
            run("Release", "main", "9988776655", JobStatus::Ok),
        ];
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// Screen position rendering `visible_rows()[row]`, via the geometry the
    /// last render stored.
    fn screen_pos(app: &App, row: usize) -> (u16, u16) {
        let line = app
            .status
            .line_rows
            .iter()
            .position(|r| *r == Some(row))
            .expect("row is on screen");
        let y = app.status.body.y + (line as u16 - app.status.scroll);
        (app.status.body.x + 1, y)
    }

    #[test]
    fn mouse_wheel_scrolls_the_status_cursor() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        let before = app.status.cursor;
        app.handle(mouse_scroll(true, 5, 5));
        let after = app.status.cursor;
        assert!(after > before, "wheel down advanced the cursor");
        app.handle(mouse_scroll(false, 5, 5));
        assert!(app.status.cursor < after, "wheel up moved it back");
    }

    #[test]
    fn clicking_a_file_row_selects_it() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        // pick a non-zero file row to prove the click maps there, not just to 0
        let rows = app.visible_rows();
        let target = rows
            .iter()
            .position(|r| matches!(r, Row::File { .. }))
            .expect("a file row");
        let (x, y) = screen_pos(&app, target);
        app.handle(mouse_click(x, y));
        assert_eq!(app.status.cursor, target);
    }

    #[test]
    fn single_click_on_a_section_header_only_selects() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        let Some(Row::SectionHeader { section, .. }) = app.visible_rows().first().cloned() else {
            panic!("first row is a section header");
        };
        let folded = app.is_folded(section);
        let (x, y) = screen_pos(&app, 0);
        app.handle(mouse_click(x, y));
        assert_eq!(app.status.cursor, 0);
        assert_eq!(app.is_folded(section), folded, "single click does not fold");
    }

    #[test]
    fn double_clicking_a_section_header_toggles_its_fold() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        let Some(Row::SectionHeader { section, .. }) = app.visible_rows().first().cloned() else {
            panic!("first row is a section header");
        };
        let folded = app.is_folded(section);
        let (x, y) = screen_pos(&app, 0);
        app.handle(mouse_click(x, y));
        app.handle(mouse_click(x, y));
        assert_eq!(app.status.cursor, 0);
        assert_ne!(
            app.is_folded(section),
            folded,
            "double-click toggled the fold"
        );
    }

    #[test]
    fn double_clicking_the_recent_commits_header_toggles_it() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        let target = app
            .visible_rows()
            .iter()
            .position(|r| matches!(r, Row::RecentHeader { .. }))
            .expect("a recent-commits header");
        let folded = app.status.recent_folded;
        let (x, y) = screen_pos(&app, target);
        app.handle(mouse_click(x, y));
        app.handle(mouse_click(x, y));
        assert_eq!(app.status.cursor, target);
        assert_ne!(
            app.status.recent_folded, folded,
            "double-click toggled the fold"
        );
    }

    fn app_for(fixture: &Fixture) -> App {
        App::new(fixture.review(), LoadedConfig::default())
    }

    fn cursor_to_file(app: &mut App, section: Section) {
        let rows = app.visible_rows();
        app.status.cursor = rows
            .iter()
            .position(|row| matches!(row, Row::File { section: s, .. } if *s == section))
            .expect("file row");
    }

    #[test]
    fn status_screen_renders() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn status_screen_renders_as_a_tree_when_configured() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.status_file_layout = crate::config::FileLayout::Tree;
        let mut app = App::new(fixture.review(), loaded);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn folded_sections_show_headers_only() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        // fold each section: cursor lands back on the header after a fold,
        // so one j reaches the next section's header
        for _ in 0..3 {
            app.handle(key('\t'));
            app.handle(key('j'));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn expanded_file_shows_inline_hunks_with_emphasis() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('\t'));
        let terminal = render(&mut app);
        // the text snapshot carries no styles: assert the intra-line
        // emphasis backgrounds made it into the buffer separately
        let styles = format!("{:?}", terminal.backend().buffer());
        let add_emph = format!("{:?}", app.theme.add_emph_bg);
        let del_emph = format!("{:?}", app.theme.del_emph_bg);
        assert!(styles.contains(&add_emph), "added emphasis bg rendered");
        assert!(styles.contains(&del_emph), "deleted emphasis bg rendered");
        // the inline diff is syntax-highlighted like the diff pane: the lazy
        // cache filled for the expanded rust file produced styled ranges
        let lib = app
            .status
            .highlights
            .get("src/lib.rs")
            .expect("expanded file highlighted");
        assert!(
            lib.new.iter().any(|line| !line.is_empty()),
            "rust syntax produced styled ranges for the inline diff"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn same_file_staged_and_unstaged_appears_in_both_sections() {
        let fixture = standard_fixture();
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n");
        fixture.stage("src/lib.rs");
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    42 + 0\n}\n");
        let mut app = app_for(&fixture);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn cursor_on_a_diff_line_highlights_the_row() {
        let fixture = two_hunk_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('\t'));
        app.handle(key('}'));
        app.handle(key('j'));
        assert!(matches!(
            app.visible_rows()[app.status.cursor],
            Row::DiffLine { .. }
        ));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn confirm_dialog_renders_over_the_status_screen() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('x'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn viewed_file_renders_collapsed_with_a_check_mark() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('\t'));
        app.handle(key('v'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn recent_commits_unfold_to_oid_and_subject_rows() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let rows = app.visible_rows();
        app.status.cursor = rows
            .iter()
            .position(|row| matches!(row, Row::RecentHeader { .. }))
            .expect("recent header");
        app.handle(key('\t'));
        // pin "now" an hour past the newest commit so the ages render stably
        app.now_unix = app
            .status
            .recent
            .iter()
            .map(|e| e.time_unix)
            .max()
            .unwrap_or(0)
            + 3661;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn collapsed_dir_chain_renders_its_joined_name() {
        let fixture = Fixture::new();
        fixture.write("keep.txt", "x\n");
        fixture.commit_all("initial commit");
        // a single-child chain: docs/ -> api/ -> intro.md
        fixture.write("docs/api/intro.md", "# intro\n");
        let mut app = app_for(&fixture);
        let screen = render(&mut app).backend().to_string();
        assert!(
            screen.contains("docs/api"),
            "the collapsed chain shows its joined name, not just the last segment:\n{screen}"
        );
    }

    #[test]
    fn clean_repo_renders_empty_state_with_recent_commits() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    41\n}\n");
        fixture.commit_all("initial commit");
        let mut app = app_for(&fixture);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn status_bar_shows_message() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        // sending feedback surfaces an info message in the bar
        app.handle(key('Z'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn status_search_highlights_a_matching_row() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        // "o" matches todo.md (the cursor lands there) and the Recent commits
        // header, whose matched letters carry the search background
        app.handle(key('/'));
        app.handle(key('o'));
        app.handle(key('\n'));
        let terminal = render(&mut app);
        let buffer = format!("{:?}", terminal.backend().buffer());
        assert!(
            buffer.contains(&format!("{:?}", app.theme.search)),
            "a non-cursor match carries the search background"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn file_row_highlights_only_the_matched_substring() {
        let fixture = standard_fixture();
        // flat layout renders the whole path "src/lib.rs"
        let app = app_for(&fixture);
        let file = app.section_files(Section::Unstaged).first().expect("file");
        // "lib" sits at bytes 4..7 of "src/lib.rs"
        let spans = super::file_spans(&app, Some(file), &app.theme, 0, &[(4..7, true)]);
        let highlighted: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.bg == Some(app.theme.search_current))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            highlighted,
            vec!["lib"],
            "only the matched word, not the whole row: {spans:?}"
        );
    }

    #[test]
    fn help_popup_lists_the_active_keymap_and_transient_groups() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('?'));
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        // top-level leaves still list their action names
        assert!(content.contains("open_review_diff"), "{content}");
        // transients appear as a prefix line plus their grouped sub-keys
        assert!(content.contains("Commit …"), "{content}");
        assert!(content.contains("Amend"), "{content}");
        assert!(content.contains("Branch …"), "{content}");
        assert!(content.contains("Create and checkout"), "{content}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn hint_line_reflects_config_remaps() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded
            .config
            .keys
            .status
            .insert("stage".to_owned(), "<c-s>".to_owned());
        let mut app = App::new(fixture.review(), loaded);
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("<c-s> stage"), "{content}");
        assert!(!content.contains(" s stage"), "{content}");
    }

    #[test]
    fn which_key_branch_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('b'));
        // the reveal timer has not elapsed: no panel yet (no flash)
        assert!(app.which_key_panel().is_none());
        app.handle(AppEvent::Tick);
        assert!(app.which_key_panel().is_some());
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn which_key_commit_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('c'));
        app.handle(AppEvent::Tick);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn which_key_push_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('P'));
        app.handle(AppEvent::Tick);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn which_key_fetch_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('f'));
        app.handle(AppEvent::Tick);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_fast_resolving_key_never_flashes_the_panel() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('b'));
        // resolving before the reveal tick: the panel is never shown and the
        // transient closes
        assert!(app.which_key_panel().is_none());
        app.handle(key('n'));
        assert!(app.transient.is_none(), "n resolved create");
        assert!(app.which_key_panel().is_none());
    }

    #[test]
    fn prefix_only_hint_line_shows_no_sub_commands() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let content = render(&mut app).backend().to_string();
        let hint = content.lines().next().unwrap_or_default();
        assert!(hint.contains("c commit"), "{hint}");
        assert!(hint.contains("b branch"), "{hint}");
        assert!(hint.contains("l log"), "{hint}");
        assert!(hint.contains("P push"), "{hint}");
        assert!(hint.contains("p pull"), "{hint}");
        assert!(hint.contains("f fetch"), "{hint}");
        // sub-commands (amend/reword/checkout/upstream) stay out of the hint line
        assert!(!hint.contains("amend"), "{hint}");
        assert!(!hint.contains("checkout"), "{hint}");
        assert!(!hint.contains("upstream"), "{hint}");
    }

    #[test]
    fn branch_list_modal_renders_with_the_head_marker() {
        let fixture = standard_fixture();
        fixture.branch("feat/topic");
        let mut app = app_for(&fixture);
        app.handle(key('b'));
        app.handle(key('b'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn cursor_highlight_moves_with_the_cursor() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        // styles live in the buffer debug output, not the text view
        app.handle(key('j'));
        let moved = format!("{:?}", render(&mut app).backend().buffer());
        app.handle(key('k'));
        let back = format!("{:?}", render(&mut app).backend().buffer());
        assert_ne!(moved, back, "cursor movement must change the rendered rows");
    }

    #[test]
    fn body_scrolls_to_keep_the_cursor_visible() {
        let fixture = two_hunk_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('\t'));
        app.handle(key('G'));
        let backend = TestBackend::new(120, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("draw");
        let content = terminal.backend().to_string();
        assert!(
            content.contains("Recent commits"),
            "view follows the cursor to the bottom: {content}"
        );
    }

    #[test]
    fn ticks_do_not_change_the_screen() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let before = render(&mut app).backend().to_string();
        app.handle(AppEvent::Tick);
        let after = render(&mut app).backend().to_string();
        assert_eq!(before, after);
    }

    use crate::theme::Theme;
    use crate::ui::{diffstat_spans, proportion_bar, status_color};
    use diffler_core::model::FileStatus;

    fn bar_cells(spans: &[ratatui::text::Span<'_>], fg: ratatui::style::Color) -> usize {
        spans
            .iter()
            .filter(|s| s.style.fg == Some(fg))
            .map(|s| s.content.chars().count())
            .sum()
    }

    #[test]
    fn proportion_bar_is_empty_without_changes() {
        let theme = Theme::github_dark();
        assert!(proportion_bar(&theme, 0, 0, theme.bg).is_empty());
    }

    #[test]
    fn proportion_bar_fills_five_cells_split_by_ratio() {
        let theme = Theme::github_dark();
        for (add, del) in [(5, 0), (0, 5), (8, 4), (1, 100), (100, 1)] {
            let spans = proportion_bar(&theme, add, del, theme.bg);
            let green = bar_cells(&spans, theme.added);
            let red = bar_cells(&spans, theme.error_fg);
            assert_eq!(green + red, 5, "({add},{del}) must fill 5 cells");
            // a non-zero side always keeps at least one cell so it stays visible
            assert_eq!(add > 0, green > 0, "({add},{del}) green visibility");
            assert_eq!(del > 0, red > 0, "({add},{del}) red visibility");
        }
    }

    #[test]
    fn status_color_distinguishes_the_status_groups() {
        let theme = Theme::github_dark();
        assert_eq!(status_color(&theme, FileStatus::Added), theme.added);
        assert_eq!(status_color(&theme, FileStatus::Untracked), theme.added);
        assert_eq!(status_color(&theme, FileStatus::Deleted), theme.error_fg);
        assert_eq!(status_color(&theme, FileStatus::Modified), theme.warn_fg);
        assert_eq!(status_color(&theme, FileStatus::Renamed), theme.warn_fg);
    }

    #[test]
    fn diffstat_spans_color_each_side_and_dim_a_zero() {
        let theme = Theme::github_dark();
        assert!(diffstat_spans(&theme, 0, 0, theme.bg).is_empty());

        let spans = diffstat_spans(&theme, 3, 0, theme.bg);
        assert_eq!(spans[0].content, " +3");
        assert_eq!(spans[0].style.fg, Some(theme.added));
        assert_eq!(spans[1].content, " -0");
        assert_eq!(spans[1].style.fg, Some(theme.dim), "a zero side is dimmed");

        let spans = diffstat_spans(&theme, 0, 7, theme.bg);
        assert_eq!(spans[0].style.fg, Some(theme.dim));
        assert_eq!(spans[1].style.fg, Some(theme.error_fg));
    }

    #[test]
    fn tree_file_row_glyph_is_colored_by_status_and_shows_the_basename() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.status_file_layout = crate::config::FileLayout::Tree;
        let app = App::new(fixture.review(), loaded);
        // the unstaged section holds a modified file in the standard fixture
        let file = app.section_files(Section::Unstaged).first().expect("file");
        let spans = super::file_spans(&app, Some(file), &app.theme, 1, &[]);
        let glyph = spans
            .iter()
            .find(|s| s.content.trim() == file.status.glyph().to_string())
            .expect("status glyph span");
        assert_eq!(glyph.style.fg, Some(status_color(&app.theme, file.status)));
        // the tree shows the basename, not the full path
        assert!(
            spans.iter().any(|s| s.content == "lib.rs"),
            "basename present: {spans:?}"
        );
        assert!(
            spans.iter().all(|s| s.content != file.path),
            "full path dropped: {spans:?}"
        );
    }

    #[test]
    fn flat_list_file_row_shows_the_full_repo_relative_path() {
        let fixture = standard_fixture();
        // default layout is the flat magit list
        let app = app_for(&fixture);
        let file = app.section_files(Section::Unstaged).first().expect("file");
        let spans = super::file_spans(&app, Some(file), &app.theme, 0, &[]);
        // the whole path shows, not just the basename
        assert!(
            spans.iter().any(|s| s.content == file.path),
            "full path present: {spans:?}"
        );
        assert!(
            spans.iter().all(|s| s.content != "lib.rs"),
            "basename alone not shown: {spans:?}"
        );
    }
}
