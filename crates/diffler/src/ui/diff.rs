//! Diff/review screen: a two-pane layout — a left file sidebar listing every
//! file in the diff (status, viewed mark, comment count) and a right pane that
//! renders the visible slice of the selected file's hunks, lines, and inline
//! comment blocks, keeping the cursor in view.

use std::collections::HashMap;
use std::sync::OnceLock;

use diffler_core::highlight::Highlighter;
use diffler_core::model::{DiffModel, FileDiff};
use diffler_core::session::{Comment, CommentStatus, Session};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::{
    App, CommentLine, DiffRow, DiffSource, DiffView, FileHighlights, Pane, comment_display,
};
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::diff_render::{file_gutter_width, hunk_header, line_syntax, render_diff_line};
use crate::ui::{diffstat_spans, hint_line, proportion_bar, status_bar, status_color};

/// Hint entries, rendered against the live keymap so remaps show.
const HINTS: &[(&[Action], &str)] = &[
    (&[Action::ToggleFocus], "focus"),
    (&[Action::Comment], "comment"),
    (&[Action::VisualSelect], "select"),
    (&[Action::Reply], "reply"),
    (&[Action::Resolve], "resolve"),
    (&[Action::MarkViewed], "viewed"),
    (&[Action::NextFile, Action::PrevFile], "file"),
    (&[Action::CopyFileFeedback, Action::CopyAllFeedback], "copy"),
    (&[Action::Back], "back"),
];

/// Sidebar width: a quarter of the screen, clamped to a readable band.
fn sidebar_width(total: u16) -> u16 {
    (total / 4).clamp(28, 44).min(total)
}

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

    // attach intra-line emphasis to the selected file once, before the
    // read-only borrows below; the model is computed here on first access
    app.enrich_diff_selected_file();

    // disjoint field borrows: the diff view mutates (scroll, highlight
    // cache) while theme and review stay read-only
    let theme = &app.theme;
    let review = &app.review;
    if let Some(diff) = app.diff.as_mut() {
        diff.ensure_rows(review);
        // a commit view renders from its pinned model; only fall back to the
        // (lazily computed) working-tree model for the working-tree view
        let review_model = (diff.commit_model.is_none()).then(|| review.model());
        draw_body(frame, body, theme, &review.session, review_model, diff);
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
    review_model: Option<&DiffModel>,
    diff: &mut DiffView,
) {
    let width = sidebar_width(area.width);
    let [list_area, pane_area] =
        Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(area);
    draw_sidebar(frame, list_area, theme, session, review_model, diff);
    draw_pane(frame, pane_area, theme, session, review_model, diff);
}

/// Left pane: one row per file in the diff, the selected one highlighted, the
/// focused pane's border accented.
fn draw_sidebar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    session: &Session,
    review_model: Option<&DiffModel>,
    diff: &DiffView,
) {
    let focused = diff.focus == Pane::List;
    let block = pane_block(theme, "Files", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(model) = diff.commit_model.as_ref().or(review_model) else {
        return;
    };
    let is_working_tree = diff.source == DiffSource::WorkingTree;
    let lines: Vec<Line<'static>> = model
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let viewed = is_working_tree && session.is_viewed(&file.path, &file.content_hash());
            let open = open_comment_count(session, &file.path);
            sidebar_line(
                theme,
                file,
                viewed,
                open,
                inner.width,
                index == diff.selected,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Right pane: the selected file's header then the visible slice of its rows.
fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    session: &Session,
    review_model: Option<&DiffModel>,
    diff: &mut DiffView,
) {
    let focused = diff.focus == Pane::Diff;
    let block = pane_block(theme, "Diff", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(model) = diff.commit_model.as_ref().or(review_model) else {
        frame.render_widget(
            Paragraph::new(Line::styled(
                " nothing to review",
                Style::new().fg(theme.dim).bg(theme.bg),
            )),
            inner,
        );
        return;
    };
    let Some(file) = model.files.get(diff.selected) else {
        frame.render_widget(
            Paragraph::new(Line::styled(
                " nothing to review",
                Style::new().fg(theme.dim).bg(theme.bg),
            )),
            inner,
        );
        return;
    };

    // header is fixed; the rows scroll beneath it
    let [header_area, rows_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let is_working_tree = diff.source == DiffSource::WorkingTree;
    let viewed = is_working_tree && session.is_viewed(&file.path, &file.content_hash());
    let open = open_comment_count(session, &file.path);
    let total = session
        .comments
        .iter()
        .filter(|c| c.anchor.file == file.path)
        .count();
    frame.render_widget(
        Paragraph::new(pane_header_line(
            theme,
            file,
            viewed,
            (open, total),
            header_area.width,
        )),
        header_area,
    );

    let height = rows_area.height.max(1) as usize;
    diff.viewport = rows_area.height;
    if diff.cursor < diff.scroll {
        diff.scroll = diff.cursor;
    }
    if diff.cursor >= diff.scroll + height {
        diff.scroll = diff.cursor + 1 - height;
    }
    let scroll = diff.scroll;
    let cursor = diff.cursor;
    let selection = diff.selection();
    // syntax is filled lazily, only for files that actually scroll into view
    ensure_file_highlights(&mut diff.highlights, file);

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
                rows_area.width,
                selected(index),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows_area);
}

/// Bordered pane with an accent title/border when focused, dim otherwise.
fn pane_block(theme: &Theme, title: &str, focused: bool) -> Block<'static> {
    let border = if focused { theme.accent } else { theme.border };
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border).bg(theme.bg))
        .title(Span::styled(
            format!(" {title} "),
            Style::new()
                .fg(if focused { theme.accent } else { theme.dim })
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ))
}

fn open_comment_count(session: &Session, path: &str) -> usize {
    session
        .comments
        .iter()
        .filter(|c| c.anchor.file == path && c.status != CommentStatus::Resolved)
        .count()
}

fn sidebar_line(
    theme: &Theme,
    file: &FileDiff,
    viewed: bool,
    open: usize,
    width: u16,
    selected: bool,
) -> Line<'static> {
    let bg = if selected {
        theme.cursor_line
    } else {
        theme.bg
    };
    let dim = Style::new().fg(theme.dim).bg(bg);
    // a left bar makes the cursor file unmistakable where the bg tint is subtle
    let marker = if selected { "▌" } else { " " };
    let glyph = file.status.glyph();
    let mut spans = vec![
        Span::styled(marker.to_owned(), Style::new().fg(theme.accent).bg(bg)),
        Span::styled(
            format!("{glyph} "),
            Style::new().fg(status_color(theme, file.status)).bg(bg),
        ),
    ];
    // reserve room for the trailing markers, then fit the path into the rest.
    // " ·{open}" is 2 + the count's digits wide; " ✓" is 2
    let suffix_width = usize::from(viewed) * 2
        + if open > 0 {
            2 + open.to_string().len()
        } else {
            0
        };
    let used = spans.iter().map(Span::width).sum::<usize>() + suffix_width;
    let room = (width as usize).saturating_sub(used + 1);
    let name = Style::new()
        .fg(if selected { theme.accent } else { theme.fg })
        .bg(bg);
    let (dir, base) = split_path(&file.path, room);
    if !dir.is_empty() {
        spans.push(Span::styled(dir, Style::new().fg(theme.dim).bg(bg)));
    }
    spans.push(Span::styled(base, name));
    if viewed {
        spans.push(Span::styled(" ✓".to_owned(), dim));
    }
    if open > 0 {
        spans.push(Span::styled(format!(" ·{open}"), dim));
    }
    pad_line(spans, bg, width)
}

/// Split a path into a dim directory prefix and a bright basename so the file
/// name stays the visible identity. The basename is preserved whole and only
/// end-clipped when it alone overflows `room`; any leftover room shows as much
/// of the directory as fits, elided from the left. Char-based, multibyte-safe.
///
/// Returns `(directory_prefix, basename)`. The directory keeps its trailing
/// `/` and is empty when nothing fits or the path is root-level.
fn split_path(path: &str, room: usize) -> (String, String) {
    let (dir, base) = match path.rfind('/') {
        Some(slash) => (&path[..=slash], &path[slash + 1..]),
        None => ("", path),
    };
    let base_width = base.chars().count();
    if room == 0 {
        return (String::new(), String::new());
    }
    // the basename is the identity: clip it only when it alone overflows
    if base_width > room {
        let keep = room.saturating_sub(1);
        let head: String = base.chars().take(keep).collect();
        return (String::new(), format!("{head}…"));
    }
    let dir_room = room - base_width;
    let dir_width = dir.chars().count();
    if dir.is_empty() || dir_room <= 1 {
        return (String::new(), base.to_owned());
    }
    if dir_width <= dir_room {
        return (dir.to_owned(), base.to_owned());
    }
    // keep the rightmost slice of the directory, room for a leading ellipsis
    let keep = dir_room - 1;
    let tail: String = dir.chars().skip(dir_width - keep).collect();
    (format!("…{tail}"), base.to_owned())
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
    match row {
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
            let syntax = highlights
                .get(&file.path)
                .and_then(|cached| line_syntax(&cached.old, &cached.new, line));
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

pub(crate) fn ensure_file_highlights(cache: &mut HashMap<String, FileHighlights>, file: &FileDiff) {
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

/// Right-pane header: status, path, binary/viewed marks, comment count.
fn pane_header_line(
    theme: &Theme,
    file: &FileDiff,
    viewed: bool,
    // (open or replied, total) comment counts for the file
    comments: (usize, usize),
    width: u16,
) -> Line<'static> {
    let bg = theme.panel;
    let dim = Style::new().fg(theme.dim).bg(bg);
    let mode = file.status.label();
    let mut spans = vec![Span::styled(format!(" {mode:<10}"), dim)];
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
    // GitHub-PR style: the file's `+A -B` and its proportion bar hug the right
    // edge of the header, mirroring the status screen's grand-total summary
    let (added, deleted) = file.diffstat();
    let mut tail = diffstat_spans(theme, added, deleted, bg);
    let bar = proportion_bar(theme, added, deleted, bg);
    if !bar.is_empty() {
        tail.push(Span::styled(" ".to_owned(), Style::new().bg(bg)));
        tail.extend(bar);
    }
    let tail_width: usize = tail.iter().map(Span::width).sum();
    let used: usize = spans.iter().map(Span::width).sum();
    let gap = (width as usize).saturating_sub(used + tail_width);
    if gap > 0 {
        spans.push(Span::styled(" ".repeat(gap), Style::new().bg(bg)));
    }
    spans.extend(tail);
    pad_line(spans, bg, width)
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

fn pad_line(mut spans: Vec<Span<'static>>, bg: Color, width: u16) -> Line<'static> {
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

    use super::split_path;
    use crate::app::{App, DiffRow, Pane};
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, key, standard_fixture};

    #[test]
    fn deep_path_keeps_full_basename_and_left_elides_the_directory() {
        let (dir, base) = split_path(
            "src/very/deeply/nested/module/paths/authentication_handler.rs",
            32,
        );
        assert_eq!(base, "authentication_handler.rs");
        assert!(
            dir.starts_with('…'),
            "directory elided from the left: {dir:?}"
        );
        assert!(
            dir.ends_with('/'),
            "trailing slash kept before basename: {dir:?}"
        );
        assert!(dir.len() > 1, "some directory context shown: {dir:?}");
        assert!(
            dir.chars().count() + base.chars().count() <= 32,
            "rendered path fits the width: {dir:?}{base:?}"
        );
    }

    #[test]
    fn basename_longer_than_width_is_end_clipped() {
        let (dir, base) = split_path("src/a_very_long_filename_indeed.rs", 12);
        assert_eq!(dir, "");
        assert_eq!(base.chars().count(), 12);
        assert!(base.ends_with('…'), "basename clipped at the end: {base:?}");
        assert!(
            base.starts_with("a_very"),
            "basename kept from the start: {base:?}"
        );
    }

    #[test]
    fn short_path_shows_full_text_without_ellipsis() {
        let (dir, base) = split_path("src/lib.rs", 32);
        assert_eq!(dir, "src/");
        assert_eq!(base, "lib.rs");
    }

    #[test]
    fn root_level_file_shows_only_the_basename() {
        let (dir, base) = split_path("todo.md", 32);
        assert_eq!(dir, "");
        assert_eq!(base, "todo.md");
    }

    #[test]
    fn directory_dropped_when_only_the_basename_fits() {
        // width holds the basename but leaves no room for any directory
        let (dir, base) = split_path("src/deeply/nested/file.rs", 8);
        assert_eq!(dir, "");
        assert_eq!(base, "file.rs");
    }

    #[test]
    fn basename_exactly_filling_width_is_not_clipped() {
        // room equal to the basename width fits whole, no room for a directory
        let (dir, base) = split_path("src/lib.rs", 6);
        assert_eq!(dir, "");
        assert_eq!(base, "lib.rs");
    }

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

    /// Select src/lib.rs and focus the diff pane.
    fn open_lib_diff(app: &mut App) {
        let index = app
            .diff
            .as_ref()
            .unwrap()
            .model(&app.review)
            .files
            .iter()
            .position(|f| f.path == "src/lib.rs")
            .expect("src/lib.rs present");
        let diff = app.diff.as_mut().unwrap();
        diff.selected = index;
        diff.focus = Pane::Diff;
        diff.invalidate();
        diff.ensure_rows(&app.review);
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
    fn diff_pane_renders_with_syntax_emphasis_and_gutter() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let terminal = render(&mut app);
        // emphasis backgrounds composited over the line backgrounds
        let styles = format!("{:?}", terminal.backend().buffer());
        let add_emph = format!("{:?}", app.theme.add_emph_bg);
        let del_emph = format!("{:?}", app.theme.del_emph_bg);
        assert!(styles.contains(&add_emph), "added emphasis bg rendered");
        assert!(styles.contains(&del_emph), "deleted emphasis bg rendered");
        // the lazy cache highlighted the selected rust file
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
    fn sidebar_focus_renders_the_file_list_and_first_file_diff() {
        let (_fixture, mut app) = diff_app();
        assert_eq!(app.diff.as_ref().unwrap().focus, Pane::List);
        insta::assert_snapshot!(render(&mut app).backend());
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
        open_lib_diff(&mut app);
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
        open_lib_diff(&mut app);
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
    fn viewed_file_shows_a_check_and_comment_count_in_the_sidebar() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        // a comment on the selected file
        app.handle(key('c'));
        for c in "look".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        // mark src/lib.rs viewed without advancing the selection off it
        let hash = app
            .review
            .model()
            .files
            .iter()
            .find(|f| f.path == "src/lib.rs")
            .map(diffler_core::model::FileDiff::content_hash)
            .unwrap();
        app.review.session.mark_viewed("src/lib.rs", &hash);
        app.diff.as_mut().unwrap().invalidate();
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        assert!(
            content.contains("✓"),
            "viewed check in the sidebar: {content}"
        );
        assert!(
            content.contains("·1"),
            "comment count in the sidebar: {content}"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn file_header_shows_resolved_when_all_comments_are_resolved() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
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
    fn viewed_walk_advances_the_selection() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('v'));
        app.handle(key('v'));
        // two files viewed; the sidebar cursor sits on the last unviewed
        // file, progress reads 2/3
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn diff_pane_renders_a_sliding_window_and_jumps_to_extremes() {
        // a single file with ~2000 lines: the model dwarfs the viewport
        let fixture = Fixture::new();
        let lines: Vec<String> = (1..=2000).map(|i| format!("line {i}")).collect();
        fixture.write("big.txt", &(lines.join("\n") + "\n"));
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

        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("big.txt");
        let total_rows = app.diff.as_ref().unwrap().rows().len();
        assert!(
            total_rows > 200,
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
        assert!(top.contains("line 1"), "{top}");
        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.scroll, 0);
        assert!(diff.highlights.contains_key("big.txt"));

        // G: the cursor lands on the last row and the window slides down
        app.handle(key('G'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, total_rows - 1);
        let bottom = render(&mut app);
        let diff = app.diff.as_ref().unwrap();
        let viewport = usize::from(diff.viewport);
        assert!(viewport < total_rows, "sanity: window smaller than model");
        assert_eq!(
            diff.scroll,
            total_rows - viewport,
            "scroll pins the cursor to the last body row"
        );
        assert!(bottom.contains("2000"), "tail row visible: {bottom}");

        // gg: back to the first row, the window slides up on the next render
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0);
        let top_again = render(&mut app);
        assert!(top_again.contains("line 1"), "{top_again}");
        assert_eq!(app.diff.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn comment_input_modal_renders_over_the_diff() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "why".chars() {
            app.handle(key(c));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }
}
