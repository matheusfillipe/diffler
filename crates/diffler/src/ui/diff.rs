//! Diff/review screen: renders the visible slice of the flattened row list
//! — file headers, hunk headers, composite diff lines, and inline comment
//! blocks — and keeps the cursor in view.

use std::collections::HashMap;
use std::sync::OnceLock;

use diffler_core::highlight::Highlighter;
use diffler_core::model::{DiffModel, FileDiff, FileStatus, LineKind};
use diffler_core::session::{Comment, CommentStatus, Session};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{
    App, CommentLine, DiffRow, DiffSource, DiffView, FileHighlights, comment_display,
};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::diff_render::{file_gutter_width, hunk_header, render_diff_line};
use crate::ui::{hint_line, status_bar};

/// Hint entries, rendered against the live keymap so remaps show.
const HINTS: &[(&[Action], &str)] = &[
    (&[Action::Comment], "comment"),
    (&[Action::VisualSelect], "select"),
    (&[Action::Reply], "reply"),
    (&[Action::Resolve], "resolve"),
    (&[Action::MarkViewed], "viewed"),
    (&[Action::ToggleFold], "fold"),
    (&[Action::CopyFileFeedback, Action::CopyAllFeedback], "copy"),
    (&[Action::Back], "back"),
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

    // disjoint field borrows: the diff view mutates (scroll, highlight
    // cache) while theme and review stay read-only
    let theme = &app.theme;
    let review = &app.review;
    if let Some(diff) = app.diff.as_mut() {
        diff.ensure_rows(review);
        draw_body(frame, body, theme, &review.session, &review.model, diff);
    }

    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

/// The process-wide syntax set: loading it is expensive, highlight results
/// are cached per file on the view.
fn highlighter() -> &'static Highlighter {
    static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
    HIGHLIGHTER.get_or_init(Highlighter::default)
}

fn draw_body(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    session: &Session,
    review_model: &DiffModel,
    diff: &mut DiffView,
) {
    let height = area.height.max(1) as usize;
    diff.viewport = area.height;
    if diff.cursor < diff.scroll {
        diff.scroll = diff.cursor;
    }
    if diff.cursor >= diff.scroll + height {
        diff.scroll = diff.cursor + 1 - height;
    }
    let scroll = diff.scroll;
    let cursor = diff.cursor;
    let selection = diff.selection();
    let model = diff.commit_model.as_ref().unwrap_or(review_model);

    // syntax is filled lazily, only for files that actually scroll into view
    let visible_files: Vec<usize> = diff
        .rows()
        .iter()
        .skip(scroll)
        .take(height)
        .filter_map(|row| match row {
            DiffRow::File { file } | DiffRow::Hunk { file, .. } | DiffRow::Line { file, .. } => {
                Some(*file)
            }
            DiffRow::Comment { .. } => None,
        })
        .collect();
    for file in visible_files {
        if let Some(file) = model.files.get(file) {
            ensure_file_highlights(&mut diff.highlights, file);
        }
    }

    let selected = |index: usize| {
        index == cursor || selection.is_some_and(|(start, end)| index >= start && index <= end)
    };
    let lines: Vec<Line<'static>> = diff
        .rows()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(index, row)| {
            row_line(
                theme,
                session,
                model,
                diff,
                row,
                area.width,
                selected(index),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn row_line(
    theme: &Theme,
    session: &Session,
    model: &DiffModel,
    diff: &DiffView,
    row: &DiffRow,
    width: u16,
    selected: bool,
) -> Line<'static> {
    let highlights = &diff.highlights;
    let is_working_tree = diff.source == DiffSource::WorkingTree;
    match row {
        DiffRow::File { file } => match model.files.get(*file) {
            Some(file) => {
                let viewed = is_working_tree && session.is_viewed(&file.path, &file.content_hash());
                let (open, total) = session
                    .comments
                    .iter()
                    .filter(|c| c.anchor.file == file.path)
                    .fold((0, 0), |(open, total), c| {
                        let live = usize::from(c.status != CommentStatus::Resolved);
                        (open + live, total + 1)
                    });
                file_header_line(
                    theme,
                    file,
                    diff.is_folded(&file.path),
                    viewed,
                    (open, total),
                    width,
                    selected,
                )
            }
            None => Line::default(),
        },
        DiffRow::Hunk { file, hunk } => {
            match model.files.get(*file).and_then(|f| f.hunks.get(*hunk)) {
                Some(hunk) => hunk_header(theme, hunk, width, selected),
                None => Line::default(),
            }
        }
        DiffRow::Line { file, hunk, line } => {
            let Some(file) = model.files.get(*file) else {
                return Line::default();
            };
            let Some(line) = file.hunks.get(*hunk).and_then(|h| h.lines.get(*line)) else {
                return Line::default();
            };
            let syntax = highlights.get(&file.path).and_then(|cached| {
                let (side, number) = match line.kind {
                    LineKind::Deleted => (&cached.old, line.old_no),
                    LineKind::Added | LineKind::Context => (&cached.new, line.new_no),
                };
                let index = usize::try_from(number?).ok()?.checked_sub(1)?;
                side.get(index).map(Vec::as_slice)
            });
            render_diff_line(
                theme,
                line,
                syntax,
                file_gutter_width(file),
                width,
                selected,
            )
        }
        DiffRow::Comment {
            comment,
            line,
            outdated,
        } => match session.comments.get(*comment) {
            Some(comment) => comment_row_line(theme, comment, *line, *outdated, width, selected),
            None => Line::default(),
        },
    }
}

fn ensure_file_highlights(cache: &mut HashMap<String, FileHighlights>, file: &FileDiff) {
    // both sides are highlighted, so the validity hash must cover both:
    // an old-side-only change (e.g. a rebase) must invalidate the entry
    let hash = file.sides_hash();
    if cache
        .get(&file.path)
        .is_some_and(|cached| cached.hash == hash)
    {
        return;
    }
    let highlight = |text: &Option<String>| {
        text.as_deref()
            .map(|content| highlighter().highlight(&file.path, content))
            .unwrap_or_default()
    };
    let entry = FileHighlights {
        hash,
        old: highlight(&file.old_text),
        new: highlight(&file.new_text),
    };
    cache.insert(file.path.clone(), entry);
}

fn file_header_line(
    theme: &Theme,
    file: &FileDiff,
    folded: bool,
    viewed: bool,
    // (open or replied, total) comment counts for the file
    comments: (usize, usize),
    width: u16,
    selected: bool,
) -> Line<'static> {
    let bg = if selected {
        theme.cursor_line
    } else {
        theme.panel
    };
    let dim = Style::new().fg(theme.dim).bg(bg);
    let marker = if folded { " ▸ " } else { " ▾ " };
    let mut spans = vec![Span::styled(marker.to_owned(), dim)];
    let mode = mode_text(file.status);
    if !mode.is_empty() {
        spans.push(Span::styled(format!("{mode:<10}"), dim));
    }
    spans.push(Span::styled(
        file.path.clone(),
        Style::new()
            .fg(theme.accent)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    ));
    if file.binary {
        spans.push(Span::styled(" (binary)".to_owned(), dim));
    }
    if viewed {
        spans.push(Span::styled(" ✓ viewed".to_owned(), dim));
    }
    // resolved-only files read as done: no count, just a quiet marker
    let (open, total) = comments;
    if open > 0 {
        let noun = if open == 1 { "comment" } else { "comments" };
        spans.push(Span::styled(format!(" · {open} {noun}"), dim));
    } else if total > 0 {
        spans.push(Span::styled(" · resolved".to_owned(), dim));
    }
    pad_line(spans, bg, width)
}

fn mode_text(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "new file",
        FileStatus::Modified => "modified",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        FileStatus::Untracked => "untracked",
    }
}

fn comment_row_line(
    theme: &Theme,
    comment: &Comment,
    line: usize,
    outdated: bool,
    width: u16,
    selected: bool,
) -> Line<'static> {
    let bg = if selected {
        theme.cursor_line
    } else {
        theme.panel
    };
    let border = Style::new().fg(theme.dim).bg(bg);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let lines = comment_display(comment);
    let Some(part) = lines.get(line) else {
        return Line::default();
    };
    let spans = match part {
        CommentLine::Header => {
            let (status, color) = match comment.status {
                CommentStatus::Open => ("open", theme.warn_fg),
                CommentStatus::Replied => ("replied", theme.accent),
                CommentStatus::Resolved => ("resolved", theme.dim),
            };
            let mut spans = vec![
                Span::styled("   ┌─ ".to_owned(), border),
                Span::styled(
                    comment.author.clone(),
                    Style::new()
                        .fg(theme.purple)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · ".to_owned(), border),
                Span::styled(status.to_owned(), Style::new().fg(color).bg(bg)),
            ];
            if outdated {
                spans.push(Span::styled(
                    " · outdated".to_owned(),
                    Style::new().fg(theme.warn_fg).bg(bg),
                ));
            }
            spans
        }
        CommentLine::Body(text) => vec![
            Span::styled("   │ ".to_owned(), border),
            Span::styled(text.clone(), fg),
        ],
        CommentLine::Reply {
            author,
            text,
            first,
        } => {
            let mut spans = vec![Span::styled("   │ ".to_owned(), border)];
            if *first {
                spans.push(Span::styled(
                    format!("↳ {author}: "),
                    Style::new().fg(theme.purple).bg(bg),
                ));
            } else {
                spans.push(Span::styled("  ".to_owned(), border));
            }
            spans.push(Span::styled(text.clone(), fg));
            spans
        }
        CommentLine::Footer => vec![Span::styled("   └─".to_owned(), border)],
    };
    pad_line(spans, bg, width)
}

fn pad_line(mut spans: Vec<Span<'static>>, bg: ratatui::style::Color, width: u16) -> Line<'static> {
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = (width as usize).saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::new().bg(bg)));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::{App, DiffRow};
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, key, standard_fixture};

    fn render(app: &mut App) -> Terminal<TestBackend> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, app))
            .expect("draw");
        terminal
    }

    fn diff_app() -> (crate::test_support::Fixture, App) {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        (fixture, app)
    }

    fn cursor_to_added_line(app: &mut App) {
        let diff = app.diff.as_ref().unwrap();
        let model = diff.model(&app.review);
        let position = diff
            .rows()
            .iter()
            .position(|row| {
                let DiffRow::Line { file, hunk, line } = row else {
                    return false;
                };
                model.files.get(*file).is_some_and(|f| {
                    f.path == "src/lib.rs"
                        && f.hunks.get(*hunk).is_some_and(|h| {
                            h.lines
                                .get(*line)
                                .is_some_and(|l| l.new_no.is_some() && l.text.contains("42"))
                        })
                })
            })
            .expect("added line");
        app.diff.as_mut().unwrap().cursor = position;
    }

    #[test]
    fn working_tree_diff_renders_with_syntax_emphasis_and_gutter() {
        let (_fixture, mut app) = diff_app();
        let terminal = render(&mut app);
        // emphasis backgrounds composited over the line backgrounds
        let styles = format!("{:?}", terminal.backend().buffer());
        let add_emph = format!("{:?}", app.theme.add_emph_bg);
        let del_emph = format!("{:?}", app.theme.del_emph_bg);
        assert!(styles.contains(&add_emph), "added emphasis bg rendered");
        assert!(styles.contains(&del_emph), "deleted emphasis bg rendered");
        // the lazy cache highlighted the visible rust file
        let highlights = &app.diff.as_ref().unwrap().highlights;
        let lib = highlights
            .get("src/lib.rs")
            .expect("src/lib.rs highlighted");
        assert!(
            lib.new.iter().any(|line| !line.is_empty()),
            "rust syntax produced styled ranges"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn commit_diff_opened_from_the_log_renders() {
        let (_fixture, mut app) = diff_app();
        // back to status, into the log, open the only commit
        app.handle(key('q'));
        app.handle(key('l'));
        app.handle(key('l'));
        app.handle(key('\n'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comment_blocks_render_open_and_replied_threads() {
        let (_fixture, mut app) = diff_app();
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "why 42?".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        let answered = app
            .review
            .session
            .add_comment(
                "reviewer",
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    hunk: None,
                    line_text: Some("pub fn answer() -> u32 {".to_owned()),
                },
                "rename this?",
            )
            .id
            .clone();
        app.review
            .session
            .reply(&answered, "agent", "kept for api compat");
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn visual_selection_highlights_the_selected_rows() {
        let (_fixture, mut app) = diff_app();
        cursor_to_added_line(&mut app);
        app.handle(key('V'));
        app.handle(key('j'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
        let terminal = render(&mut app);
        // the selection bg covers more cells than the bare cursor line does
        let bg = format!("{:?}", app.theme.cursor_line);
        let selected = format!("{:?}", terminal.backend().buffer())
            .matches(&bg)
            .count();
        app.diff.as_mut().unwrap().visual_anchor = None;
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
    fn folded_viewed_file_renders_collapsed_with_marks() {
        let (_fixture, mut app) = diff_app();
        cursor_to_added_line(&mut app);
        app.handle(key('v'));
        assert!(app.diff.as_ref().unwrap().is_folded("src/lib.rs"));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn file_header_shows_resolved_when_all_comments_are_resolved() {
        let (_fixture, mut app) = diff_app();
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "why 42?".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        let id = app.review.session.comments[0].id.clone();
        app.review.session.resolve(&id);
        app.diff.as_mut().unwrap().invalidate();
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        assert!(
            content.contains("· resolved"),
            "all-resolved file shows the marker: {content}"
        );
        assert!(
            !content.contains("1 comment"),
            "resolved comments do not count: {content}"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn status_bar_shows_viewed_progress() {
        let (_fixture, mut app) = diff_app();
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("viewed 0/3 files"), "{content}");
        app.handle(key('v'));
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("viewed 1/3 files"), "{content}");
    }

    #[test]
    fn viewed_walk_folds_files_and_advances_the_cursor() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('v'));
        app.handle(key('v'));
        // two files viewed and folded; the cursor sits on the last
        // unviewed file header, progress reads 2/3
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn huge_diff_renders_a_sliding_window_and_jumps_to_extremes() {
        // ~1800 rows across 200 hunks: edits every 10th line stay further
        // apart than the hunk-merge distance (2 × 3 context lines)
        let fixture = Fixture::new();
        let lines: Vec<String> = (1..=2000).map(|i| format!("line {i}")).collect();
        fixture.write("big.txt", &(lines.join("\n") + "\n"));
        fixture.write("tail.rs", "fn tail() -> u32 {\n    1\n}\n");
        fixture.commit_all("initial commit");
        let edited: Vec<String> = (1..=2000)
            .map(|i| {
                if i % 10 == 0 {
                    format!("edited {i}")
                } else {
                    format!("line {i}")
                }
            })
            .collect();
        fixture.write("big.txt", &(edited.join("\n") + "\n"));
        fixture.write("tail.rs", "fn tail() -> u32 {\n    2\n}\n");

        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let total_rows = app.diff.as_ref().unwrap().rows().len();
        assert!(
            total_rows > 1500,
            "the model must dwarf the viewport: {total_rows}"
        );

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut render = |app: &mut App| {
            terminal
                .draw(|frame| crate::ui::draw(frame, app))
                .expect("draw");
            terminal.backend().to_string()
        };

        let top = render(&mut app);
        assert!(top.contains("big.txt"), "{top}");
        assert!(
            !top.contains("tail.rs"),
            "rows far below the window stay unrendered: {top}"
        );
        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.scroll, 0);
        // the lazy syntax cache only fills for files the window touched
        assert!(diff.highlights.contains_key("big.txt"));
        assert!(!diff.highlights.contains_key("tail.rs"));

        // G: the cursor lands on the last row and the window slides down
        app.handle(key('G'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, total_rows - 1);
        let bottom = render(&mut app);
        assert!(bottom.contains("fn tail() -> u32 {"), "{bottom}");
        assert!(
            !bottom.contains("big.txt"),
            "the top header scrolled out: {bottom}"
        );
        let diff = app.diff.as_ref().unwrap();
        let viewport = usize::from(diff.viewport);
        assert!(viewport < total_rows, "sanity: window smaller than model");
        assert_eq!(
            diff.scroll,
            total_rows - viewport,
            "scroll pins the cursor to the last body row"
        );
        assert!(
            diff.highlights.contains_key("tail.rs"),
            "scrolling in fills the lazy cache"
        );

        // gg: back to the first row
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0);
        let top_again = render(&mut app);
        assert!(top_again.contains("big.txt"), "{top_again}");
        assert_eq!(app.diff.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn comment_input_modal_renders_over_the_diff() {
        let (_fixture, mut app) = diff_app();
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "why".chars() {
            app.handle(key(c));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }
}
