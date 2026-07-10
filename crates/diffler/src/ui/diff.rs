//! Diff/review screen: a two-pane layout — a left file sidebar listing every
//! file in the diff (status, viewed mark, comment count) and a right pane that
//! renders the visible slice of the selected file's hunks, lines, and inline
//! comment blocks, keeping the cursor in view.

use std::collections::HashMap;
use std::sync::OnceLock;

use diffler_core::highlight::{Highlighter, StyledRange, SyntaxTheme};
use diffler_core::model::{DiffLine, DiffModel, FileDiff};
use diffler_core::session::{Comment, CommentStatus, Session};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::{
    App, CommentLine, DiffRow, DiffSource, DiffView, FileHighlights, FileScope, Pane, SplitRow,
    comment_display,
};
use crate::keymap::Action;
use crate::search::Search;
use crate::theme::Theme;
use crate::tree::{Bucket, TreeNode};
use crate::ui::Hint;
use crate::ui::diff_render::{
    SplitSide, file_gutter_width, hunk_header, line_syntax, render_diff_line, render_split_pair,
};
use crate::ui::{diffstat_spans, hint_line, proportion_bar, status_bar, status_color};

/// Hint entries, rendered against the live keymap so remaps show.
const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Comment], "comment"),
    Hint::Leaf(&[Action::Reply], "reply"),
    Hint::Leaf(&[Action::MarkViewed], "viewed"),
    Hint::Leaf(&[Action::CommentsOverview], "comments"),
    Hint::Leaf(&[Action::Help], "help"),
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

    // enrichment (emphasis/highlight/scope) runs on the blocking pool; this
    // only queues work, and the pane renders plain until the result lands
    app.queue_enrich_selected();

    // disjoint field borrows: the diff view mutates (scroll, highlight
    // cache) while theme and review stay read-only
    let theme = &app.theme;
    let review = &app.review;
    let blast = &app.blast;
    let search = app.search.as_ref();
    if let Some(diff) = app.diff.as_mut() {
        diff.ensure_rows(review);
        // a commit view renders from its pinned model; only fall back to the
        // (lazily computed) working-tree model for the working-tree view
        let review_model = (diff.commit_model.is_none()).then(|| review.model());
        let session = review.session_for(&diff.source);
        let ctx = RenderCtx {
            theme,
            session,
            review_model,
            blast,
            blast_inflight: &app.blast_inflight,
            search,
        };
        draw_body(frame, body, &ctx, diff);
    }

    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

/// The process-wide syntax set: loading it is expensive, highlight results
/// are cached per file on the view.
static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();

/// Pin the shared highlighter to the session's theme. Called once at startup;
/// the theme is fixed per session, so a second call is a no-op. Because the
/// cell is process-global, a test that wants themed-syntax output must set its
/// theme before any other `App` in the same test binary pins a different one.
pub(crate) fn init_highlighter(syntax: SyntaxTheme) {
    let _ = HIGHLIGHTER.set(Highlighter::new(syntax));
}

pub fn highlighter() -> &'static Highlighter {
    HIGHLIGHTER.get_or_init(Highlighter::default)
}

struct RenderCtx<'a> {
    theme: &'a Theme,
    session: &'a Session,
    review_model: Option<&'a DiffModel>,
    blast: &'a std::collections::HashMap<String, crate::app::blast::FileBlast>,
    blast_inflight: &'a std::collections::HashSet<String>,
    search: Option<&'a Search>,
}

fn draw_body(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let (theme, session, review_model, search) =
        (ctx.theme, ctx.session, ctx.review_model, ctx.search);
    let width = sidebar_width(area.width);
    let [list_area, pane_area] =
        Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(area);
    draw_sidebar(frame, list_area, theme, session, review_model, diff, search);
    draw_pane(frame, pane_area, ctx, diff);
}

/// Left pane: one row per file in the diff, the selected one highlighted, the
/// focused pane's border accented.
fn draw_sidebar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    session: &Session,
    review_model: Option<&DiffModel>,
    diff: &mut DiffView,
    search: Option<&Search>,
) {
    let focused = diff.focus == Pane::List;
    let block = pane_block(theme, "Files", focused);
    let inner = block.inner(area);
    diff.sidebar = inner;
    frame.render_widget(block, area);
    let Some(model) = diff.commit_model.as_ref().or(review_model) else {
        return;
    };
    // build only the visible slice: the tree can be far taller than the pane
    // and styling every row per frame is O(files)
    let height = inner.height.max(1) as usize;
    let scroll = diff.tree_cursor.saturating_sub(height - 1);
    diff.sidebar_scroll = scroll;
    let lines: Vec<Line<'static>> = diff
        .tree_rows(model, session)
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(row_index, row)| {
            let on_cursor = row_index == diff.tree_cursor;
            // ranges are offsets into the row's name, so the `/` match
            // highlights the exact substring like the log and diff panes do
            let ranges = search
                .filter(|_| focused)
                .map(|s| s.ranges_for(row_index))
                .unwrap_or_default();
            match &row.node {
                TreeNode::Dir { name, path } => sidebar_dir_line(
                    theme,
                    name,
                    diff.folded_dirs.contains(path),
                    row.depth,
                    inner.width,
                    on_cursor,
                    &ranges,
                ),
                TreeNode::Section {
                    bucket,
                    count,
                    folded,
                } => sidebar_section_line(theme, *bucket, *count, *folded, inner.width, on_cursor),
                TreeNode::File { index, name } => {
                    let Some(file) = model.files.get(*index) else {
                        return Line::default();
                    };
                    let viewed = session.is_viewed(&file.path, &file.content_hash());
                    let open = open_comment_count(session, &file.path);
                    sidebar_file_line(
                        theme,
                        file,
                        name,
                        viewed,
                        open,
                        row.depth,
                        inner.width,
                        on_cursor,
                        &ranges,
                    )
                }
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Right pane: the selected file's header then the visible slice of its rows,
/// in unified or side-by-side mode.
#[allow(clippy::too_many_lines)]
fn draw_pane(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let (theme, session, review_model, blast, search) = (
        ctx.theme,
        ctx.session,
        ctx.review_model,
        ctx.blast,
        ctx.search,
    );
    let focused = diff.focus == Pane::Diff;
    let title = pane_title(&diff.source);
    let block = pane_block(theme, &title, focused);
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
    let [header_area, body_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let viewed = session.is_viewed(&file.path, &file.content_hash());
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
            blast
                .get(&file.path)
                .filter(|b| b.hash == file.sides_hash()),
            ctx.blast_inflight.contains(&file.sides_hash()),
            header_area.width,
        )),
        header_area,
    );

    // the breadcrumb row is reserved only for files that have definitions, so
    // plain files keep their full height
    let has_scope = diff
        .scopes
        .get(&file.path)
        .is_some_and(|s| !s.index.is_empty());
    let (crumb_area, rows_area) = if has_scope {
        let [crumb, rows] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body_area);
        (Some(crumb), rows)
    } else {
        (None, body_area)
    };

    diff.viewport = rows_area.height;
    diff.pane = rows_area;

    if diff.side_by_side {
        let split = diff.split_rows.clone();
        let (sel, side) = diff.split_cursor(&split);
        let height = rows_area.height.max(1) as usize;
        if sel < diff.split_scroll {
            diff.split_scroll = sel;
        }
        if sel >= diff.split_scroll + height {
            diff.split_scroll = sel + 1 - height;
        }
        let scroll = diff.split_scroll;
        let highlights = diff.highlights.get(&file.path);
        let gutter = file_gutter_width(file);
        let lines: Vec<Line<'static>> = split
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(index, row)| {
                split_row_line(
                    theme,
                    session,
                    file,
                    highlights,
                    gutter,
                    rows_area.width,
                    row,
                    index == sel,
                    side,
                )
            })
            .collect();
        let top = split
            .iter()
            .skip(scroll)
            .find_map(|r| split_right_new_no(file, r));
        render_scope_crumb(frame, crumb_area, theme, diff.scopes.get(&file.path), top);
        frame.render_widget(Paragraph::new(lines), rows_area);
        return;
    }

    let height = rows_area.height.max(1) as usize;
    diff.scroll = super::scroll_to_cursor(diff.cursor, diff.scroll, height);
    let scroll = diff.scroll;
    let cursor = diff.cursor;
    let selection = diff.selection();

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
            // only the focused pane highlights; otherwise the sidebar's
            // matches (keyed by row index) would bleed onto diff rows
            let ranges = search
                .filter(|_| focused)
                .map(|s| s.ranges_for(index))
                .unwrap_or_default();
            row_line(
                theme,
                session,
                model,
                diff,
                row,
                rows_area.width,
                selected(index),
                &ranges,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows_area);

    let top = diff
        .rows()
        .iter()
        .skip(scroll)
        .find_map(|r| row_new_no(file, r));
    render_scope_crumb(frame, crumb_area, theme, diff.scopes.get(&file.path), top);
}

#[allow(clippy::too_many_arguments)]
fn split_row_line(
    theme: &Theme,
    session: &Session,
    file: &FileDiff,
    highlights: Option<&FileHighlights>,
    gutter: usize,
    width: u16,
    row: &SplitRow,
    on_cursor: bool,
    cursor_side: Option<SplitSide>,
) -> Line<'static> {
    match *row {
        SplitRow::Hunk { hunk } => match file.hunks.get(hunk) {
            Some(hunk) => hunk_header(theme, hunk, width, on_cursor),
            None => Line::default(),
        },
        SplitRow::Pair { hunk, left, right } => {
            let Some(hunk) = file.hunks.get(hunk) else {
                return Line::default();
            };
            let cell = |index: Option<usize>, side: SplitSide| {
                index.and_then(|i| hunk.lines.get(i)).map(|line| {
                    (
                        line,
                        highlights.and_then(|hl| split_side_syntax(hl, line, side)),
                        line_annotated(session, &file.path, line),
                    )
                })
            };
            let sel_left = on_cursor && !matches!(cursor_side, Some(SplitSide::Right));
            let sel_right = on_cursor && !matches!(cursor_side, Some(SplitSide::Left));
            render_split_pair(
                theme,
                cell(left, SplitSide::Left),
                cell(right, SplitSide::Right),
                gutter,
                width,
                sel_left,
                sel_right,
            )
        }
        SplitRow::Comment {
            comment,
            line,
            outdated,
        } => match session.comments.get(comment) {
            Some(comment) => comment_row_line(theme, comment, line, outdated, width, on_cursor),
            None => Line::default(),
        },
    }
}

/// Per-side syntax for a split row: the old highlights for the left column, the
/// new highlights for the right, indexed by that side's line number.
fn split_side_syntax<'a>(
    highlights: &'a FileHighlights,
    line: &DiffLine,
    side: SplitSide,
) -> Option<&'a [StyledRange]> {
    let (column, number) = match side {
        SplitSide::Left => (&highlights.old, line.old_no),
        SplitSide::Right => (&highlights.new, line.new_no),
    };
    let index = usize::try_from(number?).ok()?.checked_sub(1)?;
    column.get(index).map(Vec::as_slice)
}

/// Diff-pane title: a plain "Diff" for the working tree or a single commit, a
/// `oldest7..newest7` range span when the pane shows a combined commit range.
fn pane_title(source: &DiffSource) -> String {
    match source {
        DiffSource::WorkingTree | DiffSource::Commit { .. } => "Diff".to_owned(),
        DiffSource::Range { oldest, newest } => {
            let short = |oid: &str| oid.get(..7).unwrap_or(oid).to_owned();
            format!("Diff {}..{}", short(oldest), short(newest))
        }
        DiffSource::Pr { number } => format!("PR #{number}"),
    }
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

/// Sidebar leading cells: the cursor `▌` marker plus the tree indent for
/// `depth`. Shared by dir and file rows so columns line up.
fn tree_lead(theme: &Theme, depth: usize, bg: Color, on_cursor: bool) -> Span<'static> {
    // a left bar makes the cursor row unmistakable where the bg tint is subtle
    let marker = if on_cursor { "▌" } else { " " };
    Span::styled(
        format!("{marker}{}", " ".repeat(depth * 2)),
        Style::new().fg(theme.accent).bg(bg),
    )
}

/// A directory row: indent, fold arrow, and the dim directory name.
#[allow(clippy::too_many_arguments)]
fn sidebar_dir_line(
    theme: &Theme,
    name: &str,
    folded: bool,
    depth: usize,
    width: u16,
    on_cursor: bool,
    search: &[(std::ops::Range<usize>, bool)],
) -> Line<'static> {
    let bg = if on_cursor {
        theme.cursor_line
    } else {
        theme.bg
    };
    let arrow = if folded { "▸ " } else { "▾ " };
    let name_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    let mut spans = vec![
        tree_lead(theme, depth, bg, on_cursor),
        Span::styled(arrow.to_owned(), Style::new().fg(theme.dim).bg(bg)),
    ];
    // dir names are never clipped, so the highlight maps straight onto them
    spans.extend(super::highlight_spans(name, name_style, search, theme));
    pad_line(spans, bg, width)
}

/// A review-bucket header row: fold arrow, bold label, and file count.
fn sidebar_section_line(
    theme: &Theme,
    bucket: Bucket,
    count: usize,
    folded: bool,
    width: u16,
    on_cursor: bool,
) -> Line<'static> {
    let bg = if on_cursor {
        theme.cursor_line
    } else {
        theme.bg
    };
    let arrow = if folded { "▸ " } else { "▾ " };
    let label_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let spans = vec![
        tree_lead(theme, 0, bg, on_cursor),
        Span::styled(arrow.to_owned(), Style::new().fg(theme.dim).bg(bg)),
        Span::styled(bucket.label().to_owned(), label_style),
        Span::styled(format!(" ({count})"), Style::new().fg(theme.dim).bg(bg)),
    ];
    pad_line(spans, bg, width)
}

/// A file row: indent, status glyph (colored), basename, then the viewed and
/// comment-count marks and the `+A -B` diffstat. The diffstat is dropped first
/// when the sidebar is too narrow to keep the name and marks legible.
#[allow(clippy::too_many_arguments)]
fn sidebar_file_line(
    theme: &Theme,
    file: &FileDiff,
    name: &str,
    viewed: bool,
    open: usize,
    depth: usize,
    width: u16,
    on_cursor: bool,
    search: &[(std::ops::Range<usize>, bool)],
) -> Line<'static> {
    let bg = if on_cursor {
        theme.cursor_line
    } else {
        theme.bg
    };
    let dim = Style::new().fg(theme.dim).bg(bg);
    let glyph = file.status.glyph();
    let mut spans = vec![
        tree_lead(theme, depth, bg, on_cursor),
        Span::styled(
            format!("{glyph} "),
            Style::new().fg(status_color(theme, file.status)).bg(bg),
        ),
    ];
    // reserve room for the trailing markers, then clip the basename into the
    // rest. " ·{open}" is 2 + the count's digits wide; " ✓" is 2
    let suffix_width = usize::from(viewed) * 2
        + if open > 0 {
            2 + open.to_string().len()
        } else {
            0
        };
    let used = spans.iter().map(Span::width).sum::<usize>() + suffix_width;
    let room = (width as usize).saturating_sub(used + 1);
    let name_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    // highlight the whole name, then clip the spans so a match stays lit on the
    // visible part; a flat-list path (with a `/`) front-elides so its tail —
    // the basename, the file's identity — stays in view
    let highlighted = super::highlight_spans(name, name_style, search, theme);
    spans.extend(clip_spans(
        highlighted,
        room,
        name.contains('/'),
        name_style,
    ));
    if viewed {
        spans.push(Span::styled(" ✓".to_owned(), dim));
    }
    if open > 0 {
        spans.push(Span::styled(format!(" ·{open}"), dim));
    }
    // GitHub-PR style: the file's `+A -B` hugs the right edge, but only if it
    // fits after the name and marks — name + marks stay legible first
    let (added, deleted) = file.diffstat();
    let stat = diffstat_spans(theme, added, deleted, bg);
    let stat_width: usize = stat.iter().map(Span::width).sum();
    let used: usize = spans.iter().map(Span::width).sum();
    if stat_width > 0 && used + stat_width < width as usize {
        let gap = (width as usize).saturating_sub(used + stat_width);
        if gap > 0 {
            spans.push(Span::styled(" ".repeat(gap), Style::new().bg(bg)));
        }
        spans.extend(stat);
    }
    pad_line(spans, bg, width)
}

/// Clip a name's already-styled `spans` to `room` cells, preserving each span's
/// style (so a search highlight survives on the visible cells). `front` elides
/// from the left with a leading `…` — for flat-list paths, keeping the tail
/// basename in view — otherwise from the right with a trailing `…`. The ellipsis
/// takes `ellipsis_style`. Char-based, multibyte-safe.
fn clip_spans(
    spans: Vec<Span<'static>>,
    room: usize,
    front: bool,
    ellipsis_style: Style,
) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if room == 0 {
        return Vec::new();
    }
    if total <= room {
        return spans;
    }
    let keep = room - 1;
    if front {
        let skip = total - keep;
        let mut out = vec![Span::styled("…".to_owned(), ellipsis_style)];
        let mut seen = 0;
        for span in spans {
            let len = span.content.chars().count();
            let start = seen;
            seen += len;
            if seen <= skip {
                continue;
            }
            let drop_here = skip.saturating_sub(start);
            let kept: String = span.content.chars().skip(drop_here).collect();
            out.push(Span::styled(kept, span.style));
        }
        out
    } else {
        let mut out = Vec::new();
        let mut taken = 0;
        for span in spans {
            if taken >= keep {
                break;
            }
            let len = span.content.chars().count();
            let take = (keep - taken).min(len);
            let kept: String = span.content.chars().take(take).collect();
            out.push(Span::styled(kept, span.style));
            taken += len;
        }
        out.push(Span::styled("…".to_owned(), ellipsis_style));
        out
    }
}

// mirrors render_diff_line's orthogonal styling inputs plus the search ranges
#[allow(clippy::too_many_arguments)]
fn row_line(
    theme: &Theme,
    session: &Session,
    model: &DiffModel,
    diff: &DiffView,
    row: &DiffRow,
    width: u16,
    selected: bool,
    search: &[(std::ops::Range<usize>, bool)],
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
            let annotated = line_annotated(session, &file.path, line);
            render_diff_line(
                theme,
                line,
                syntax,
                file_gutter_width(file),
                width,
                selected,
                annotated,
                search,
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

/// One terminal row: the enclosing-definition breadcrumb for the top visible
/// line, styled like a hunk heading. Blank when the top line is at top level.
fn scope_line(theme: &Theme, crumbs: &[String], width: u16) -> Line<'static> {
    let text = if crumbs.is_empty() {
        String::new()
    } else {
        format!(" {}", crumbs.join(" › "))
    };
    let pad = (width as usize).saturating_sub(text.chars().count());
    Line::from(vec![
        Span::styled(text, Style::new().fg(theme.dim).bg(theme.panel)),
        Span::styled(" ".repeat(pad), Style::new().bg(theme.panel)),
    ])
}

fn render_scope_crumb(
    frame: &mut Frame<'_>,
    area: Option<Rect>,
    theme: &Theme,
    scope: Option<&FileScope>,
    top_new_line: Option<u32>,
) {
    let Some(area) = area else {
        return;
    };
    let crumbs = match (scope, top_new_line) {
        (Some(scope), Some(line)) => scope.index.crumbs(line.saturating_sub(1) as usize),
        _ => Vec::new(),
    };
    frame.render_widget(Paragraph::new(scope_line(theme, &crumbs, area.width)), area);
}

/// Whether `line` falls inside any comment's anchored range for `file_path` —
/// drives the GitHub-style highlight marking a multi-line comment's scope.
fn line_annotated(session: &Session, file_path: &str, line: &DiffLine) -> bool {
    session.comments.iter().any(|c| {
        if c.anchor.file != file_path {
            return false;
        }
        let Some(start) = c.anchor.line else {
            return false;
        };
        let end = c.anchor.line_end.unwrap_or(start);
        let no = if c.anchor.on_old_side {
            line.old_no
        } else {
            line.new_no
        };
        no.is_some_and(|n| start <= n && n <= end)
    })
}

/// New-side line number of a unified diff row, if it has one.
fn row_new_no(file: &FileDiff, row: &DiffRow) -> Option<u32> {
    match *row {
        DiffRow::Line { hunk, line, .. } => file.hunks.get(hunk)?.lines.get(line)?.new_no,
        _ => None,
    }
}

/// New-side line number of a split row's right cell, if any.
fn split_right_new_no(file: &FileDiff, row: &SplitRow) -> Option<u32> {
    match *row {
        SplitRow::Pair { hunk, right, .. } => file.hunks.get(hunk)?.lines.get(right?)?.new_no,
        _ => None,
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
    impact: Option<&crate::app::blast::FileBlast>,
    computing: bool,
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
    if computing && impact.is_none() {
        spans.push(Span::styled(" · scanning refs…".to_owned(), dim));
    }
    if let Some(blast) = impact {
        let refs: usize = blast.symbols.iter().map(|s| s.total_refs).sum();
        if refs > 0 {
            spans.push(Span::styled(
                format!(
                    " · referenced {refs}× · {} files outside diff",
                    blast.outside_files()
                ),
                Style::new().fg(theme.warn_fg).bg(bg),
            ));
        }
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
    // a solid left bar in the comment's status color turns the block into a
    // distinct card that stands out against the diff lines around it
    let (status_label, accent) = match comment.status {
        CommentStatus::Open => ("open", theme.warn_fg),
        CommentStatus::Replied => ("replied", theme.accent),
        CommentStatus::Resolved => ("resolved", theme.dim),
    };
    let bar = Span::styled("  ▌ ".to_owned(), Style::new().fg(accent).bg(bg));
    let dim = Style::new().fg(theme.dim).bg(bg);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let lines = comment_display(comment);
    let Some(part) = lines.get(line) else {
        return Line::default();
    };
    let spans = match part {
        CommentLine::Header => {
            let mut spans = vec![
                bar,
                Span::styled(
                    comment.author.clone(),
                    Style::new()
                        .fg(theme.purple)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · ".to_owned(), dim),
                Span::styled(
                    status_label.to_owned(),
                    Style::new().fg(accent).bg(bg).add_modifier(Modifier::BOLD),
                ),
            ];
            if outdated {
                spans.push(Span::styled(
                    " · outdated".to_owned(),
                    Style::new().fg(theme.warn_fg).bg(bg),
                ));
            }
            spans
        }
        CommentLine::Body(text) => vec![bar, Span::styled(text.clone(), fg)],
        CommentLine::Reply {
            author,
            text,
            first,
        } => {
            let mut spans = vec![bar];
            if *first {
                spans.push(Span::styled(
                    format!("↳ {author}: "),
                    Style::new().fg(theme.purple).bg(bg),
                ));
            } else {
                spans.push(Span::styled("  ".to_owned(), fg));
            }
            spans.push(Span::styled(text.clone(), fg));
            spans
        }
        CommentLine::Footer => vec![Span::styled(
            "  ▌".to_owned(),
            Style::new().fg(accent).bg(bg),
        )],
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

    use ratatui::style::Style;

    use super::clip_spans;
    use crate::app::{App, DiffRow, Pane};
    use crate::config::LoadedConfig;
    use crate::test_support::{
        Fixture, key, mouse_click, mouse_drag, mouse_right_click, mouse_scroll, standard_fixture,
    };
    use crate::theme::Theme;

    fn plain(text: &str) -> Vec<ratatui::text::Span<'static>> {
        vec![ratatui::text::Span::raw(text.to_owned())]
    }

    fn joined(spans: &[ratatui::text::Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn clip_spans_keeps_a_short_name_whole() {
        let style = ratatui::style::Style::new();
        assert_eq!(
            joined(&clip_spans(plain("lib.rs"), 32, false, style)),
            "lib.rs"
        );
    }

    #[test]
    fn clip_spans_end_elides_a_long_basename() {
        let style = ratatui::style::Style::new();
        let clipped = clip_spans(plain("a_very_long_filename_indeed.rs"), 12, false, style);
        let text = joined(&clipped);
        assert_eq!(text.chars().count(), 12);
        assert!(text.ends_with('…'), "clipped at the end: {text:?}");
        assert!(text.starts_with("a_very"), "kept from the start: {text:?}");
    }

    #[test]
    fn clip_spans_front_elides_a_path_to_keep_the_basename() {
        let style = ratatui::style::Style::new();
        assert_eq!(
            joined(&clip_spans(plain("src/lib.rs"), 32, true, style)),
            "src/lib.rs"
        );
        let clipped = clip_spans(plain("deep/nested/dir/module.rs"), 12, true, style);
        let text = joined(&clipped);
        assert_eq!(text.chars().count(), 12);
        assert!(text.starts_with('…'), "front-elided: {text:?}");
        assert!(text.ends_with("module.rs"), "basename kept: {text:?}");
    }

    #[test]
    fn clip_spans_zero_room_yields_nothing() {
        let style = ratatui::style::Style::new();
        assert!(clip_spans(plain("lib.rs"), 0, false, style).is_empty());
    }

    #[test]
    fn clip_spans_keeps_a_highlight_lit_on_the_visible_part() {
        let theme = Theme::github_dark();
        // "status" starts at byte 13 of this name and survives an end-clip
        let name = "diffler__ui__status__tests__foo";
        let spans = super::super::highlight_spans(name, Style::new(), &[(13..19, true)], &theme);
        let clipped = clip_spans(spans, 20, false, Style::new());
        let lit: String = clipped
            .iter()
            .filter(|s| s.style.bg == Some(theme.search_current))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            lit, "status",
            "the match stays lit after clipping: {clipped:?}"
        );
    }

    fn render(app: &mut App) -> Terminal<TestBackend> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, app))
            .expect("draw");
        // the first draw only queues enrichment; run it and draw again so the
        // snapshot captures the settled frame, as the app converges to
        app.enrich_now();
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

    #[test]
    fn review_layout_sidebar_buckets_viewed_files() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Review;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        // one viewed file in the folded bucket, two left to review
        app.handle(key('v'));
        insta::assert_snapshot!(render(&mut app).backend());
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
    fn pane_header_shows_the_impact_badge() {
        use crate::app::blast::{FileBlast, SymbolImpact};
        use crate::lsp::RefSite;
        let (_fixture, mut app) = diff_app();
        let hash = app.review.model().files[0].sides_hash();
        let path = app.review.model().files[0].path.clone();
        app.blast.insert(
            path,
            FileBlast {
                hash,
                symbols: vec![SymbolImpact {
                    total_refs: 4,
                    outside: vec![RefSite {
                        path: "src/other.rs".into(),
                        line: 2,
                    }],
                }],
            },
        );
        let content = render(&mut app).backend().to_string();
        assert!(
            content.contains("referenced 4× · 1 files outside diff"),
            "{content}"
        );
    }

    #[test]
    fn diff_pane_renders_with_syntax_emphasis_and_gutter() {
        // grapheme engine: it char-diffs the `41`→`42` literal so the emphasis
        // background composites. The syntactic engine treats the whole literal
        // as changed (no partial highlight); that path is covered by the core
        // intraline tests.
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.semantic_diff = false;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
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
    fn comment_range_highlights_its_anchored_lines() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            "reviewer",
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(1),
                line_end: Some(2),
                on_old_side: false,
                line_text: None,
            },
            "this whole block",
        );
        app.diff.as_mut().unwrap().invalidate();
        let terminal = render(&mut app);
        let styles = format!("{:?}", terminal.backend().buffer());
        assert!(
            styles.contains(&format!("{:?}", app.theme.annotated)),
            "a multi-line comment paints its anchored lines with the annotated bg"
        );
    }

    #[test]
    fn side_by_side_pane_renders_old_and_new_columns() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.diff.as_mut().unwrap().side_by_side = true;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// Sidebar file position on screen for the last render.
    fn sidebar_file_pos(app: &App) -> (u16, u16, usize) {
        let diff = app.diff.as_ref().unwrap();
        let session = app.review.session_for(&diff.source);
        let rows = diff.tree_rows(diff.model(&app.review), session);
        let target = rows
            .iter()
            .position(|r| matches!(r.node, crate::tree::TreeNode::File { .. }))
            .expect("a file row in the sidebar");
        let x = diff.sidebar.x + 1;
        let y = diff.sidebar.y + target as u16 - diff.sidebar_scroll as u16;
        (x, y, target)
    }

    #[test]
    fn single_click_in_the_sidebar_selects_without_focusing() {
        let (_fixture, mut app) = diff_app();
        render(&mut app);
        let (x, y, target) = sidebar_file_pos(&app);
        app.handle(mouse_click(x, y));
        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.tree_cursor, target);
        assert_eq!(diff.focus, Pane::List, "single click keeps sidebar focus");
    }

    #[test]
    fn double_click_in_the_sidebar_opens_the_file() {
        let (_fixture, mut app) = diff_app();
        render(&mut app);
        let (x, y, target) = sidebar_file_pos(&app);
        app.handle(mouse_click(x, y));
        app.handle(mouse_click(x, y));
        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.tree_cursor, target);
        assert_eq!(diff.focus, Pane::Diff, "double-click opens into the pane");
    }

    #[test]
    fn mouse_wheel_over_the_pane_scrolls_it() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let before = app.diff.as_ref().unwrap().cursor;
        let pane = app.diff.as_ref().unwrap().pane;
        app.handle(mouse_scroll(true, pane.x + 1, pane.y + 1));
        assert!(
            app.diff.as_ref().unwrap().cursor > before,
            "wheel advanced the pane cursor"
        );
    }

    /// The first two `DiffRow::Line` rows and their on-screen y positions.
    fn first_two_pane_lines(app: &App) -> (u16, u16, u16, usize, usize) {
        let diff = app.diff.as_ref().unwrap();
        let lines: Vec<usize> = diff
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, DiffRow::Line { .. }))
            .map(|(i, _)| i)
            .collect();
        let x = diff.pane.x + 1;
        let y0 = diff.pane.y + lines[0] as u16 - diff.scroll as u16;
        let y1 = diff.pane.y + lines[1] as u16 - diff.scroll as u16;
        (x, y0, y1, lines[0], lines[1])
    }

    #[test]
    fn dragging_in_the_pane_selects_a_line_range() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let (x, y0, y1, line0, line1) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_drag(x, y1));
        let diff = app.diff.as_ref().unwrap();
        assert!(diff.visual_anchor.is_some(), "drag started a selection");
        assert_eq!(diff.selection(), Some((line0, line1)));
    }

    #[test]
    fn double_click_in_the_pane_starts_a_comment() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let (x, y0, ..) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(x, y0));
        assert!(app.modal.is_some(), "double-click opened the comment input");
    }

    #[test]
    fn right_click_cancels_a_pane_selection() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let (x, y0, y1, ..) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_drag(x, y1));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
        app.handle(mouse_right_click(x, y0));
        assert_eq!(
            app.diff.as_ref().unwrap().visual_anchor,
            None,
            "right-click dropped the selection"
        );
    }

    #[test]
    fn sidebar_focus_renders_the_file_list_and_first_file_diff() {
        let (_fixture, mut app) = diff_app();
        assert_eq!(app.diff.as_ref().unwrap().focus, Pane::List);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn sidebar_search_does_not_bleed_into_the_diff_pane() {
        let (_fixture, mut app) = diff_app();
        assert_eq!(app.diff.as_ref().unwrap().focus, Pane::List);
        // a filename match while the sidebar is focused must not paint the pane
        app.handle(key('/'));
        for c in "lib".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let sidebar = super::sidebar_width(120);
        let search_bgs = [app.theme.search, app.theme.search_current];
        let mut highlighted = 0;
        for y in 0..40 {
            for x in 0..120 {
                if search_bgs.contains(&buffer[(x, y)].bg) {
                    assert!(
                        x < sidebar,
                        "search bg at col {x} bleeds past sidebar {sidebar}"
                    );
                    highlighted += 1;
                }
            }
        }
        assert!(
            highlighted > 0,
            "the focused sidebar should still highlight the match"
        );
    }

    #[test]
    fn sidebar_file_row_highlights_only_the_matched_substring() {
        let (_fixture, app) = diff_app();
        let file = app
            .review
            .model()
            .files
            .iter()
            .find(|f| f.path == "src/lib.rs")
            .cloned()
            .expect("src/lib.rs present");
        // a wide row so the name is not clipped; "lib" is bytes 0..3 of "lib.rs"
        let spans = super::sidebar_file_line(
            &app.theme,
            &file,
            "lib.rs",
            false,
            0,
            0,
            80,
            false,
            &[(0..3, true)],
        );
        let highlighted: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.bg == Some(app.theme.search_current))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            highlighted,
            vec!["lib"],
            "only the matched word lights up, not the whole row: {spans:?}"
        );
    }

    #[test]
    fn sidebar_renders_as_a_flat_list_when_configured() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::List;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        assert_eq!(app.diff.as_ref().unwrap().focus, Pane::List);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn sidebar_scrolls_to_keep_the_cursor_visible() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write(".keep", "x\n");
        fixture.commit_all("base");
        for i in 0..40 {
            fixture.write(&format!("f{i:02}.txt"), "x\n");
        }
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let count = {
            let diff = app.diff.as_ref().unwrap();
            let session = app.review.session_for(&diff.source);
            diff.tree_rows(diff.model(&app.review), session).len()
        };
        app.diff.as_mut().unwrap().tree_cursor = count - 1;

        let backend = TestBackend::new(120, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("draw");
        let content = terminal.backend().to_string();
        assert!(content.contains("f39.txt"), "cursor row visible: {content}");
        // f01 lives only in the sidebar (f00 is the selected file shown in the
        // pane header), so its absence proves the sidebar scrolled past the top
        assert!(
            !content.contains("f01.txt"),
            "top rows scrolled off: {content}"
        );
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
    fn range_diff_from_the_log_renders_with_a_range_header() {
        let fixture = standard_fixture();
        fixture.write("notes.txt", "alpha\nbeta\n");
        fixture.commit_all("add beta note");
        fixture.write(
            "src/util.rs",
            "pub fn twice(x: u32) -> u32 {\n    x * 2\n}\n",
        );
        fixture.commit_all("add util module");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        // into the log, select the two newest commits, open the combined diff
        app.handle(key('l'));
        app.handle(key('l'));
        app.handle(key('V'));
        app.handle(key('j'));
        app.handle(key('\n'));
        let content = render(&mut app).backend().to_string();
        // the pane title carries the oldest7..newest7 span
        assert!(
            content.contains(".."),
            "range header shows a span: {content}"
        );
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
        app.enrich_now();
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
