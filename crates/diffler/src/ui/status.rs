//! Status screen: hint line, head line, neogit-style sections with inline
//! diff expansion, recent commits, and the status bar.

use diffler_core::model::{FileDiff, FileStatus, Hunk};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Row, Section};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::diff_render::render_hunk_lines;
use crate::ui::{cursor_line, hint_line, status_bar};

/// Hint entries, rendered against the live keymap so remaps show.
const HINTS: &[(&[Action], &str)] = &[
    (&[Action::ToggleFold], "toggle"),
    (&[Action::Stage], "stage"),
    (&[Action::Unstage], "unstage"),
    (&[Action::Discard], "discard"),
    (&[Action::CommitFlow], "commit"),
    (&[Action::Help], "help"),
];

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, body_area, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(hint_line(app, HINTS)), hint);
    let (lines, scroll) = body(app, body_area);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body_area);
    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

/// Body lines plus the vertical scroll keeping the cursor row in view.
fn body(app: &App, area: Rect) -> (Vec<Line<'static>>, u16) {
    let mut lines = vec![head_line(app), Line::default()];
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
    let mut index = 0;
    while let Some(row) = rows.get(index) {
        match *row {
            // a hunk renders as one block: header + its diff lines, which all
            // follow contiguously in the flattened rows
            Row::HunkHeader {
                section,
                file,
                hunk,
            } => {
                let Some(hunk) = hunk_at(app, section, file, hunk) else {
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
                lines.extend(render_hunk_lines(&app.theme, hunk, area.width, selected));
                index += span;
            }
            row => {
                if index > 0 && matches!(row, Row::SectionHeader { .. } | Row::RecentHeader { .. })
                {
                    lines.push(Line::default());
                }
                if index == app.status.cursor {
                    cursor_line_index = lines.len();
                }
                lines.push(row_line(app, &row, index == app.status.cursor, area.width));
                index += 1;
            }
        }
    }

    let height = area.height.max(1) as usize;
    let scroll = cursor_line_index.saturating_sub(height - 1) as u16;
    (lines, scroll)
}

fn hunk_at(app: &App, section: Section, file: usize, hunk: usize) -> Option<&Hunk> {
    app.section_files(section).get(file)?.hunks.get(hunk)
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

fn row_line(app: &App, row: &Row, selected: bool, width: u16) -> Line<'static> {
    let theme = &app.theme;
    let spans = match row {
        Row::SectionHeader { section, count } => {
            header_spans(theme, section.title(), *count, app.is_folded(*section))
        }
        Row::RecentHeader { count } => {
            header_spans(theme, "Recent commits", *count, app.status.recent_folded)
        }
        Row::File { section, index } => {
            let file = app.section_files(*section).get(*index);
            file_spans(app, file, theme)
        }
        Row::Commit { index } => commit_spans(app, *index, theme),
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

fn header_spans(theme: &Theme, title: &str, count: usize, folded: bool) -> Vec<Span<'static>> {
    vec![
        Span::styled(if folded { " ▸ " } else { " ▾ " }, theme.dim_style()),
        Span::styled(
            title.to_owned(),
            Style::new()
                .fg(theme.accent)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" ({count})"), theme.dim_style()),
    ]
}

fn file_spans(app: &App, file: Option<&FileDiff>, theme: &Theme) -> Vec<Span<'static>> {
    let Some(file) = file else {
        return Vec::new();
    };
    let mut spans = vec![Span::styled("     ", theme.base())];
    let mode = mode_text(file.status);
    if !mode.is_empty() {
        spans.push(Span::styled(format!("{mode:<10}"), theme.dim_style()));
    }
    spans.push(Span::styled(file.path.clone(), theme.base()));
    if app.is_path_viewed(&file.path) {
        spans.push(Span::styled(" ✓", theme.dim_style()));
    }
    spans
}

fn commit_spans(app: &App, index: usize, theme: &Theme) -> Vec<Span<'static>> {
    let Some(entry) = app.status.recent.get(index) else {
        return Vec::new();
    };
    vec![Span::styled(
        format!("     {} {}", entry.oid7, entry.subject),
        theme.dim_style(),
    )]
}

fn mode_text(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "new file",
        FileStatus::Modified => "modified",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        // the untracked section lists bare paths, neogit-style
        FileStatus::Untracked => "",
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::{App, Row, Section};
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, key, standard_fixture, two_hunk_fixture};

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
        insta::assert_snapshot!(render(&mut app).backend());
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
    fn help_popup_lists_the_active_keymap_over_status() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('?'));
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        assert!(content.contains("commit_flow"), "{content}");
        assert!(content.contains("open_review_diff"), "{content}");
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
    fn branch_popup_renders_as_a_bottom_split_over_status() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('b'));
        insta::assert_snapshot!(render(&mut app).backend());
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
}
