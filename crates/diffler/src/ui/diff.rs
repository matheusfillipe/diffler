//! Diff/review screen: file sidebar, diff pane, and the comments or references sidebar.

use std::collections::HashMap;

use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffLine, DiffModel, FileDiff, LineKind};
use diffler_core::session::{Comment, CommentStatus, Session};
use diffler_core::source::ReviewSource;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::RefEntry;
use crate::app::composer::{Composer, ComposerKind, ComposerLine};
use crate::app::markdown::MdSpan;
use crate::app::rowsel::RowSelect;
use crate::app::walkthrough::{Block as WalkthroughBlock, stop_title, summary_figure_key};
use crate::app::{
    App, CommentFacts, CommentGrouping, CommentLine, CommentPaneRow, DiffRow, DiffView,
    FileHighlights, FileScope, Pane, RowCopy, SplitRow, SplitSide, group_comment_rows,
    summary_display,
};
use crate::config::FileLayout;
use crate::keymap::Action;
use crate::search::Search;
use crate::theme::Theme;
use crate::tree::{Bucket, TreeNode};
use crate::ui::Hint;
use crate::ui::diff_render::{
    LineFlags, Mark, PairSelection, align_scroll, card_frame, cursor_band, diff_line_height,
    file_gutter_width, fold_row, hunk_header, line_syntax, render_diff_line, render_split_pair,
    split_pair_height, syntax_row,
};
use crate::ui::{diffstat_spans, proportion_bar, status_bar, status_color};

const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Comment], "add comment"),
    Hint::Leaf(&[Action::Reply], "reply"),
    Hint::Leaf(&[Action::MarkViewed], "mark viewed"),
    Hint::Leaf(&[Action::CommentsOverview], "see comment list"),
    Hint::Leaf(&[Action::Help], "help"),
];

fn sidebar_width(total: u16) -> u16 {
    (total / 4).clamp(28, 44).min(total)
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (body, bar) = super::screen_chrome(frame, app, HINTS);

    let open_figure_hint = app.active_keymap().chord_for(Action::OpenFigureGraph);
    app.queue_enrich_selected();
    // comment bodies are parsed to this width, so we set it before anything reads them
    let pane_width = body.width.saturating_sub(sidebar_width(body.width) + 2);
    if let Some(diff) = app.diff.as_mut() {
        diff.set_wrap_width(pane_width);
    }
    app.ensure_walkthrough_view();

    // we borrow fields disjointly: the diff view mutates while theme and review stay read-only
    let theme = &app.theme;
    let review = &app.review;
    let search = app.search.as_ref();
    let highlighter = app.highlighter.as_ref();
    let human_author = app.author.as_str();
    let algorithm = app.config.diff.algorithm;
    if let Some(diff) = app.diff.as_mut() {
        diff.ensure_rows(review);
        // we clone the source so the session's borrow stays off the view, which the rasteriser mutates
        let source = diff.source.clone();
        let session = review.session_for(&source);
        // rasterising needs the graph mutably while the pane's loop borrows the model, so we do it up front
        let rasters = rasterize_figures(diff, theme, pane_width, open_figure_hint.as_deref());
        patch_figure_copy_text(diff, &rasters);
        // context files stay off this model because `diff` itself goes into `draw_body`,
        // so each renderer that needs them reads `diff.context_files`
        let review_model = (diff.commit_model.is_none()).then(|| review.model());
        let author_orders = pane_author_orders(
            session,
            diff.commit_model.as_ref().or(review_model),
            human_author,
        );
        let ctx = RenderCtx {
            theme,
            session,
            review_model,
            search,
            highlighter,
            human_author,
            rasters: &rasters,
            algorithm,
            author_orders,
        };
        draw_body(frame, body, &ctx, diff);
    }
    // we queue after drawing so the worker builds the picture for the size the pane just drew
    app.queue_image_preview();

    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

struct RenderCtx<'a> {
    theme: &'a Theme,
    session: &'a Session,
    review_model: Option<&'a DiffModel>,
    search: Option<&'a Search>,
    highlighter: &'a diffler_core::highlight::Highlighter,
    human_author: &'a str,
    rasters: &'a FigureRaster,
    algorithm: diffler_core::diffalgo::DiffAlgorithm,
    author_orders: HashMap<&'a str, usize>,
}

/// Rendered figure rows, keyed by the card's figure-cache key and block index.
type FigureRaster = HashMap<(String, usize), Vec<Line<'static>>>;

fn rasterize_figures(
    diff: &mut DiffView,
    theme: &Theme,
    width: u16,
    open_figure_hint: Option<&str>,
) -> FigureRaster {
    let mut raster = FigureRaster::new();
    for (id, cached) in &mut diff.figures {
        let mut ordinal = 0;
        for (block, part) in cached.blocks.iter_mut().enumerate() {
            let WalkthroughBlock::Figure(figure) = part else {
                continue;
            };
            ordinal += 1;
            raster.insert(
                (id.clone(), block),
                super::diff_render::figure_lines(
                    figure,
                    ordinal,
                    width,
                    theme,
                    theme.bg,
                    open_figure_hint,
                ),
            );
        }
    }
    raster
}

/// A figure row's copy text exists only once the figure is drawn, so we patch
/// each [`RowCopy::Figure`] key with the line this pass rasterised.
fn patch_figure_copy_text(diff: &mut DiffView, rasters: &FigureRaster) {
    for entry in &mut diff.row_copy {
        let RowCopy::Figure { key, block, row } = entry else {
            continue;
        };
        let Some(line) = rasters
            .get(&(key.clone(), *block))
            .and_then(|lines| lines.get(*row))
        else {
            continue;
        };
        // span 0 is the card's decorative bar; the rest is the figure itself
        let text: String = line
            .spans
            .iter()
            .skip(1)
            .map(|span| span.content.as_ref())
            .collect();
        *entry = RowCopy::Text(text);
    }
}

#[derive(Clone, Copy)]
struct RowState {
    selected: bool,
    focused: bool,
}

#[derive(Clone, Copy)]
struct TreeRowCtx<'a> {
    theme: &'a Theme,
    depth: usize,
    width: u16,
    on_cursor: bool,
    focused: bool,
    search: &'a [(std::ops::Range<usize>, bool)],
}

#[derive(Clone, Copy)]
struct SplitFileCtx<'a> {
    file: &'a FileDiff,
    highlights: Option<&'a FileHighlights>,
    gutter: usize,
}

/// Empty columns between panes; the gap is the divider.
const PANE_GAP: u16 = 1;

fn draw_body(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let width = (sidebar_width(area.width) + 1).min(area.width);
    let comments = comments_width(area.width, diff.comments_open || diff.refs_visible());
    let [list_area, _gap, pane_area, _right_gap, comments_area] = Layout::horizontal([
        Constraint::Length(width),
        Constraint::Length(PANE_GAP),
        Constraint::Min(0),
        Constraint::Length(if comments == 0 { 0 } else { PANE_GAP }),
        Constraint::Length(comments),
    ])
    .areas(area);
    draw_sidebar(frame, list_area, ctx, diff);
    draw_pane(frame, pane_area, ctx, diff);
    if comments > 0 && diff.refs_visible() {
        draw_references(frame, comments_area, ctx, diff);
    } else if comments > 0 {
        draw_comments(frame, comments_area, ctx, diff);
    }
}

#[derive(Clone, Copy)]
struct BlockLook<'a> {
    theme: &'a Theme,
    surface: Color,
    selected: bool,
    focused: bool,
    color: Color,
    width: u16,
    syntax: Option<&'a FileHighlights>,
}

/// A reference's preview with its shared indent cut, the use's own line bright
/// and the name tinted, the rest dimmed.
fn reference_block(look: BlockLook<'_>, entry: &RefEntry) -> Vec<Line<'static>> {
    let BlockLook {
        theme,
        surface,
        selected,
        focused,
        color,
        width,
        syntax,
    } = look;
    let (bg, _) = card_frame(theme, selected, focused, color);
    let margin = Span::styled("  ".to_owned(), Style::new().bg(surface));
    let bar = Span::styled("▌ ".to_owned(), Style::new().fg(color).bg(bg));
    let indent = entry
        .preview
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .map(|line| line.text.len() - line.text.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (at, line) in entry.preview.iter().enumerate() {
        let gutter = line
            .number
            .map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
        let (mark, mark_fg) = match line.kind {
            LineKind::Added => ("+ ", theme.added),
            LineKind::Deleted => ("- ", theme.error_fg),
            LineKind::Context => ("  ", theme.dim),
        };
        let code = line.text.trim_end().get(indent..).unwrap_or_default();
        let text = crate::text::elide(code, usize::from(width).saturating_sub(11));
        let own = at == entry.own;
        let shift = |range: &std::ops::Range<usize>| {
            range.start.saturating_sub(indent)..range.end.saturating_sub(indent)
        };
        let spans: Vec<StyledRange> = syntax
            .and_then(|highlights| {
                let side = if line.kind == LineKind::Deleted {
                    &highlights.old
                } else {
                    &highlights.new
                };
                syntax_row(side, line.number)
            })
            .map(|ranges| {
                ranges
                    .iter()
                    .filter(|styled| styled.range.end > indent)
                    .map(|styled| StyledRange {
                        range: shift(&styled.range),
                        ..styled.clone()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let marks = if own {
            vec![(shift(&entry.range), Mark::Lens(color))]
        } else {
            Vec::new()
        };
        let mut row = vec![
            margin.clone(),
            bar.clone(),
            Span::styled(gutter, Style::new().fg(theme.dim).bg(bg)),
            Span::styled(mark, Style::new().fg(mark_fg).bg(bg)),
        ];
        let code_spans =
            super::diff_render::composite_spans(theme, &text, &[], Some(&spans), bg, bg, &marks);
        row.extend(code_spans.into_iter().map(|span| {
            if own {
                span
            } else {
                let style = span.style.add_modifier(Modifier::DIM);
                span.style(style)
            }
        }));
        lines.push(pad_line(row, bg, width));
    }
    lines
}

/// A file's header in the references sidebar: its folder dimmed and its
/// name bright, cut from the front so the name always shows, the way a path
/// row in the file sidebar reads.
fn reference_header(
    theme: &Theme,
    bg: Color,
    width: u16,
    path: &str,
    count: usize,
) -> Line<'static> {
    let dim = Style::new().fg(theme.dim).bg(bg);
    let name = Style::new().fg(theme.fg).bg(bg);
    let count = format!(" ({count})");
    let mut spans = vec![
        tree_lead(theme, 0, bg, false),
        Span::styled("▾ ".to_owned(), dim),
    ];
    let used = spans.iter().map(Span::width).sum::<usize>() + count.len();
    let room = usize::from(width).saturating_sub(used + 1);
    let parent = path.rfind('/').map_or(0, |at| at + 1);
    let styled = super::highlight_spans_split(path, parent, dim, name, &[], theme);
    spans.extend(clip_spans(styled, room, true, dim));
    spans.push(Span::styled(count, dim));
    pad_line(spans, bg, width)
}

/// Right pane while a lens name is focused: its uses in diff order, grouped by file.
fn draw_references(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let theme = ctx.theme;
    let surface = sidebar_bg(theme);
    let focused = diff.focus == Pane::References;
    frame.render_widget(Block::new().style(Style::new().bg(surface)), area);
    let [heading, inner] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    diff.comments_rect = inner;
    diff.ref_lines.clear();
    let Some(lens) = diff.lens.as_mut() else {
        return;
    };
    let Some((slot, symbol)) = lens
        .view
        .focus
        .and_then(|slot| lens.data.symbols.get(slot).map(|symbol| (slot, symbol)))
    else {
        return;
    };
    let view = &mut lens.view;
    let title = format!("References · {} ({})", symbol.name, view.refs.len());
    frame.render_widget(
        Paragraph::new(pane_heading(theme, &title, focused, surface)),
        heading,
    );
    if view.refs.is_empty() {
        let hint = Line::styled(
            " esc close the lens",
            Style::new().fg(theme.dim).bg(surface),
        );
        frame.render_widget(Paragraph::new(vec![hint]), inner);
        return;
    }
    let color = lens_color(theme, slot);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut owners: Vec<Option<usize>> = Vec::new();
    let mut selected_span = (0, 1);
    for (at, entry) in view.refs.iter().enumerate() {
        if let Some(count) = entry.group_len {
            lines.push(reference_header(
                theme,
                surface,
                inner.width,
                &entry.path,
                count,
            ));
            owners.push(Some(at));
        }
        let selected = at == view.ref_cursor;
        let start = lines.len();
        let look = BlockLook {
            theme,
            surface,
            selected,
            focused,
            color,
            width: inner.width,
            syntax: diff.highlights.get(&entry.path),
        };
        lines.extend(reference_block(look, entry));
        lines.push(Line::default());
        owners.resize(lines.len(), Some(at));
        if selected {
            selected_span = (start, lines.len() - start);
        }
    }
    let height = usize::from(inner.height);
    view.scroll = super::scroll_to_span(
        selected_span.0,
        selected_span.1,
        view.scroll,
        height,
        lines.len(),
    );
    let visible: Vec<Line<'static>> = lines.into_iter().skip(view.scroll).take(height).collect();
    diff.ref_lines = owners;
    frame.render_widget(
        Paragraph::new(visible).style(Style::new().bg(surface)),
        inner,
    );
}

fn comments_width(total: u16, open: bool) -> u16 {
    if !open {
        return 0;
    }
    sidebar_width(total).min(total / 3)
}

#[derive(Clone, Copy)]
struct CardSearch<'a> {
    query: &'a str,
    /// This card holds the active match, so its hits take the stronger colour.
    current: bool,
}

#[derive(Clone, Copy)]
struct CardCtx<'a> {
    theme: &'a Theme,
    budget: usize,
    bg: Color,
    width: u16,
    depth: usize,
    on_cursor: bool,
    orphan: bool,
    author_color: Color,
    search: Option<CardSearch<'a>>,
}

/// Built from the same ordering `App::comment_rows` groups, so a click or a key
/// and the drawn rows agree on which row is which.
fn comment_pane_rows(
    ordered: &[(&diffler_core::session::Comment, bool)],
    diff: &DiffView,
) -> Vec<CommentPaneRow> {
    let facts: Vec<CommentFacts> = ordered
        .iter()
        .map(|(comment, orphan)| CommentFacts {
            id: comment.id.clone(),
            file: comment.anchor.file.clone(),
            author: comment.author.clone(),
            status: comment.status,
            orphan: *orphan,
        })
        .collect();
    group_comment_rows(&facts, diff.comment_grouping, &diff.comment_folds)
}

#[derive(Clone, Copy)]
struct HeaderCtx<'a> {
    theme: &'a Theme,
    bg: Color,
    width: u16,
    on_cursor: bool,
}

/// A group header row with an optional right-aligned tail (the file sidebar
/// passes a diffstat).
fn group_header_line(
    hc: HeaderCtx<'_>,
    label: &str,
    count: usize,
    folded: bool,
    tail: Vec<Span<'static>>,
) -> Line<'static> {
    let HeaderCtx {
        theme,
        bg,
        width,
        on_cursor,
    } = hc;
    let arrow = if folded { "▸ " } else { "▾ " };
    let label_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let mut spans = vec![
        tree_lead(theme, 0, bg, on_cursor),
        Span::styled(arrow.to_owned(), dim),
        Span::styled(label.to_owned(), label_style),
        Span::styled(format!(" ({count})"), dim),
    ];
    push_right(&mut spans, tail, width, bg);
    pad_line(spans, bg, width)
}

fn comment_group_header_line(
    theme: &Theme,
    bg: Color,
    width: u16,
    on_cursor: bool,
    label: &str,
    count: usize,
    folded: bool,
) -> Line<'static> {
    group_header_line(
        HeaderCtx {
            theme,
            bg,
            width,
            on_cursor,
        },
        label,
        count,
        folded,
        Vec::new(),
    )
}

/// Right pane: the review's comments under the pane's own grouping. The
/// selection drives the diff cursor, so the pane's verbs act on the highlighted card.
fn draw_comments(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let theme = ctx.theme;
    let focused = diff.focus == Pane::Comments;
    let surface = sidebar_bg(theme);
    frame.render_widget(Block::new().style(Style::new().bg(surface)), area);
    let [heading, inner] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let ordered = ordered_comments(ctx, diff);
    let rows = comment_pane_rows(&ordered, diff);
    frame.render_widget(
        Paragraph::new(pane_heading(
            theme,
            &format!(
                "Comments ({}) · {}",
                ordered.len(),
                diff.comment_grouping.label()
            ),
            focused,
            surface,
        )),
        heading,
    );
    diff.comments_rect = inner;

    let (mut lines, owners, cursor_line) = comment_pane_lines(ctx, diff, &rows, inner, focused);
    diff.comment_lines = owners;
    if lines.is_empty() {
        let dim = Style::new().fg(theme.dim).bg(surface);
        lines.push(Line::styled(" no comments yet", dim));
        lines.push(Line::styled(" c to add one", dim));
    }

    let height = inner.height.max(1) as usize;
    diff.comments_scroll =
        super::scroll_to_cursor(cursor_line, diff.comments_scroll, height, lines.len());
    let shown: Vec<Line<'static>> = lines
        .into_iter()
        .skip(diff.comments_scroll)
        .take(height)
        .collect();
    frame.render_widget(Paragraph::new(shown), inner);
}

/// The pane's rendered lines, each line's owning row (so a click on a wrapped
/// line selects its comment), and the cursor's line index.
fn comment_pane_lines(
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    rows: &[CommentPaneRow],
    inner: Rect,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<Option<usize>>, usize) {
    let theme = ctx.theme;
    let search = ctx.search;
    let surface = sidebar_bg(theme);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut owners: Vec<Option<usize>> = Vec::new();
    let budget = (inner.width as usize).saturating_sub(2).max(1);
    let item_depth = usize::from(diff.comment_grouping != CommentGrouping::Flat);
    let orders = &ctx.author_orders;
    let mut cursor_line = 0usize;
    for (row_index, row) in rows.iter().enumerate() {
        let on_cursor = row_index == diff.comments_cursor;
        if on_cursor {
            cursor_line = lines.len();
        }
        let bg = sidebar_row_bg(theme, on_cursor, focused);
        match row {
            CommentPaneRow::Header {
                label,
                count,
                folded,
                ..
            } => {
                lines.push(comment_group_header_line(
                    theme,
                    bg,
                    inner.width,
                    on_cursor,
                    label,
                    *count,
                    *folded,
                ));
                owners.push(Some(row_index));
            }
            CommentPaneRow::Item { id, orphan } => {
                let Some(comment) = ctx.session.comment(id) else {
                    continue;
                };
                let order = orders.get(comment.author.as_str()).copied().unwrap_or(0);
                let card = CardCtx {
                    theme,
                    budget,
                    bg,
                    width: inner.width,
                    depth: item_depth,
                    on_cursor,
                    orphan: *orphan,
                    author_color: author_color(theme, bg, ctx.human_author, &comment.author, order),
                    search: search.filter(|_| focused).map(|search| CardSearch {
                        query: search.query(),
                        current: search.current_row() == Some(row_index),
                    }),
                };
                // the cursor's comment stays one line too: the diff pane shows it in
                // full, and a row that grew under the cursor would shift every row below
                lines.push(comment_summary_line(&card, comment));
                owners.push(Some(row_index));
                let last_in_group =
                    !matches!(rows.get(row_index + 1), Some(CommentPaneRow::Item { .. }));
                if last_in_group {
                    lines.push(Line::styled(
                        " ".repeat(inner.width as usize),
                        Style::new().bg(surface),
                    ));
                    owners.push(None);
                }
            }
        }
    }
    (lines, owners, cursor_line)
}

/// Comments by diff file order then line, matching `App::comment_order`, each
/// paired with whether its file left the diff (orphaned, ranked last).
fn ordered_comments<'a>(
    ctx: &'a RenderCtx<'_>,
    diff: &DiffView,
) -> Vec<(&'a diffler_core::session::Comment, bool)> {
    comments_in_order(ctx.session, diff.commit_model.as_ref().or(ctx.review_model))
}

fn comments_in_order<'a>(
    session: &'a Session,
    model: Option<&DiffModel>,
) -> Vec<(&'a diffler_core::session::Comment, bool)> {
    let rank = |path: &str| {
        model
            .and_then(|model| model.files.iter().position(|file| file.path == path))
            .unwrap_or(usize::MAX)
    };
    let mut ordered: Vec<(&diffler_core::session::Comment, usize)> = session
        .comments
        .iter()
        .map(|comment| (comment, rank(&comment.anchor.file)))
        .collect();
    ordered.sort_by_key(|(comment, rank)| (*rank, comment.anchor.line.unwrap_or(0)));
    ordered
        .into_iter()
        .map(|(comment, rank)| (comment, rank == usize::MAX))
        .collect()
}

fn search_ranges(
    search: Option<CardSearch<'_>>,
    text: &str,
) -> Vec<(std::ops::Range<usize>, bool)> {
    search
        .map(|search| {
            crate::search::find_matches(&[(0, text.to_owned())], search.query)
                .into_iter()
                .map(|found| (found.range, search.current))
                .collect()
        })
        .unwrap_or_default()
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    // `h` is `hue.rem_euclid(360.0) / 60.0`, always in 0.0..6.0
    #[allow(clippy::cast_sign_loss)]
    let sector = h as u32;
    let (r1, g1, b1) = match sector {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    let channel = |v: f32| {
        // the clamp keeps the value in 0.0..=255.0 before the cast
        #[allow(clippy::cast_sign_loss)]
        let byte = ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        byte
    };
    (channel(r1), channel(g1), channel(b1))
}

fn lens_marks(
    diff: &DiffView,
    model: &DiffModel,
    row: &DiffRow,
    theme: &Theme,
) -> Vec<(std::ops::Range<usize>, Mark)> {
    let (Some(lens), DiffRow::Line { file, hunk, line }) = (diff.lens.as_ref(), *row) else {
        return Vec::new();
    };
    let Some(file) = model.files.get(file) else {
        return Vec::new();
    };
    let Some(line) = file.hunks.get(hunk).and_then(|h| h.lines.get(line)) else {
        return Vec::new();
    };
    let on_old_side = line.kind == diffler_core::model::LineKind::Deleted;
    let Some(number) = line.number_on(on_old_side) else {
        return Vec::new();
    };
    let mut marks: Vec<_> = lens
        .marks(&file.path, on_old_side, number)
        .into_iter()
        .map(|(range, symbol)| (range, Mark::Lens(lens_color(theme, symbol))))
        .collect();
    marks.extend(
        lens.labels(&file.path, on_old_side, number, &line.text)
            .into_iter()
            .map(|(range, digit, symbol)| (range, Mark::Label(digit, lens_color(theme, symbol)))),
    );
    marks
}

fn lens_color(theme: &Theme, slot: usize) -> Color {
    #[allow(clippy::cast_precision_loss)] // a hue only needs to look distinct, not be exact
    let hue = (slot as f32 * HUE_STEP + 25.0).rem_euclid(360.0);
    let (r, g, b) = hsl_to_rgb(hue, 0.7, 0.55);
    let (r, g, b) = diffler_core::language::readable_on((r, g, b), super::rgb_of(theme.bg));
    Color::Rgb(r, g, b)
}

/// The golden angle, so each new hue lands far from every earlier one.
const HUE_STEP: f32 = 137.507_76;

/// Each author's first-appearance position, skipping the human and the agent
/// since they take fixed colours.
fn author_orders<'a>(
    authors: impl Iterator<Item = &'a str>,
    human_author: &str,
) -> HashMap<&'a str, usize> {
    let mut orders = HashMap::new();
    for author in authors {
        if author == human_author || author == crate::mcp::AGENT_AUTHOR {
            continue;
        }
        let next = orders.len();
        orders.entry(author).or_insert(next);
    }
    orders
}

/// Comment authors in sidebar order first, then anyone who only replied.
fn pane_author_orders<'a>(
    session: &'a Session,
    model: Option<&DiffModel>,
    human_author: &str,
) -> HashMap<&'a str, usize> {
    let roots = comments_in_order(session, model);
    let repliers = session
        .comments
        .iter()
        .flat_map(|comment| comment.replies.iter().map(|reply| reply.author.as_str()));
    author_orders(
        roots
            .iter()
            .map(|(comment, _)| comment.author.as_str())
            .chain(repliers),
        human_author,
    )
}

/// The human and the agent keep fixed colours because the reader looks for
/// them first; everyone else steps the golden angle from `order`, lifted for
/// contrast against the row's own `bg`.
fn author_color(theme: &Theme, bg: Color, human_author: &str, author: &str, order: usize) -> Color {
    if !human_author.is_empty() && author == human_author {
        return theme.accent;
    }
    if author == crate::mcp::AGENT_AUTHOR {
        return theme.purple;
    }
    #[allow(clippy::cast_precision_loss)] // a hue only needs to look distinct, not be exact
    let hue = (order as f32 * HUE_STEP).rem_euclid(360.0);
    let (r, g, b) = hsl_to_rgb(hue, 0.55, 0.6);
    let (r, g, b) = diffler_core::language::readable_on((r, g, b), super::rgb_of(bg));
    Color::Rgb(r, g, b)
}

/// The title, else the body's first line.
fn comment_preview(comment: &diffler_core::session::Comment) -> String {
    comment.title.clone().unwrap_or_else(|| {
        comment
            .body
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    })
}

/// The status glyph and author spans, plus the width left after them.
fn comment_header_spans(
    cc: &CardCtx<'_>,
    comment: &diffler_core::session::Comment,
) -> (Vec<Span<'static>>, usize) {
    let &CardCtx {
        theme,
        budget,
        bg,
        depth,
        on_cursor,
        orphan,
        author_color,
        ..
    } = cc;
    // an orphan's file is gone, so we show that over its status
    let (status, colour) = match comment.status {
        _ if orphan => ("⚠", theme.error_fg),
        CommentStatus::Open => ("○", theme.warn_fg),
        CommentStatus::Replied => ("◐", theme.accent),
        CommentStatus::Resolved => ("✓", theme.added),
    };
    let spans = vec![
        tree_lead(theme, depth, bg, on_cursor),
        Span::styled(format!("{status} "), Style::new().fg(colour).bg(bg)),
        Span::styled(
            format!("{} ", super::elide(&comment.author, AUTHOR_MAX)),
            Style::new()
                .fg(author_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    (spans, budget.saturating_sub(used))
}

/// Caps an author's columns so a long handle leaves room for the preview.
const AUTHOR_MAX: usize = 14;

fn comment_summary_line(
    cc: &CardCtx<'_>,
    comment: &diffler_core::session::Comment,
) -> Line<'static> {
    let (mut spans, rest_budget) = comment_header_spans(cc, comment);
    let preview = super::elide(&comment_preview(comment), rest_budget);
    spans.extend(super::highlight_spans(
        &preview,
        Style::new().fg(cc.theme.dim).bg(cc.bg),
        &search_ranges(cc.search, &preview),
        cc.theme,
    ));
    pad_line(spans, cc.bg, cc.width)
}

fn draw_sidebar(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let (theme, session, review_model, search) =
        (ctx.theme, ctx.session, ctx.review_model, ctx.search);
    let focused = diff.focus == Pane::List;
    let surface = sidebar_bg(theme);
    frame.render_widget(Block::new().style(Style::new().bg(surface)), area);
    let [heading, inner] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(pane_heading(
            theme,
            &sidebar_title(ctx, diff),
            focused,
            surface,
        )),
        heading,
    );
    diff.sidebar = inner;
    let Some(model) = diff.commit_model.as_ref().or(review_model) else {
        return;
    };
    // we style only the visible slice since the tree can be far taller than the pane
    let height = inner.height.max(1) as usize;
    let rows = diff.tree_rows(model, session);
    let stat = GroupStat::collect(diff, model, session);
    let scroll = super::scroll_to_cursor(diff.tree_cursor, diff.sidebar_scroll, height, rows.len());
    diff.sidebar_scroll = scroll;
    let active_walkthrough = diff.active_walkthrough(session);
    let lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(row_index, row)| {
            let on_cursor = row_index == diff.tree_cursor;
            let ranges = search
                .filter(|_| focused)
                .map(|s| s.ranges_for(row_index))
                .unwrap_or_default();
            let row_ctx = TreeRowCtx {
                theme,
                depth: row.depth,
                width: inner.width,
                on_cursor,
                focused,
                search: &ranges,
            };
            match &row.node {
                TreeNode::Dir { name, path } => sidebar_dir_line(
                    &row_ctx,
                    name,
                    diff.folded_dirs.contains(path),
                    stat.dirs.get(path).copied().unwrap_or_default(),
                ),
                TreeNode::Section {
                    bucket,
                    count,
                    folded,
                } => sidebar_section_line(
                    &row_ctx,
                    *bucket,
                    *count,
                    stat.sections.get(bucket).copied().unwrap_or_default(),
                    *folded,
                ),
                TreeNode::File { index, name } => {
                    let Some(file) = model.files.get(*index) else {
                        return Line::default();
                    };
                    let viewed = session.is_viewed(&file.path, &file.content_hash());
                    let open = open_comment_count(session, &file.path);
                    sidebar_file_line(&row_ctx, file, name, viewed, open)
                }
                TreeNode::Stop { index } => {
                    sidebar_stop_line(&row_ctx, session, active_walkthrough, *index)
                }
                TreeNode::WalkthroughSummary => sidebar_summary_line(&row_ctx),
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// "Files", or the walkthrough's name in its layout so the reader knows whose
/// reading order it is. A broken pin concerns the whole walkthrough, so we say it here.
fn sidebar_title(ctx: &RenderCtx<'_>, diff: &DiffView) -> String {
    match (diff.layout, diff.active_walkthrough(ctx.session)) {
        (FileLayout::Walkthrough, Some(walkthrough)) => {
            let progress = crate::app::walkthrough::progress_label(ctx.session, walkthrough);
            if diff.pin_broken {
                format!("{} ({progress}, pin lost)", walkthrough.title)
            } else {
                format!("{} ({progress})", walkthrough.title)
            }
        }
        _ => "Files".to_owned(),
    }
}

fn sidebar_stop_line(
    rc: &TreeRowCtx<'_>,
    session: &Session,
    walkthrough: Option<&diffler_core::walkthrough::Walkthrough>,
    index: usize,
) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        width,
        on_cursor,
        focused,
        search,
        ..
    } = rc;
    let Some(primary) = walkthrough
        .and_then(|walkthrough| walkthrough.stops.get(index))
        .and_then(|id| session.comments.iter().position(|c| c.id == *id))
    else {
        return Line::default();
    };
    let Some(stop) = session.comments.get(primary) else {
        return Line::default();
    };
    let held = crate::app::walkthrough::slide_comments(session, primary).len();
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let title_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    // stops never re-sort by seen state, so we trail the check where a viewed file leads with it
    let mut spans = vec![tree_lead(theme, 0, bg, on_cursor)];
    spans.extend(super::highlight_spans(
        &stop_title(stop),
        title_style,
        search,
        theme,
    ));
    if session.is_stop_seen(&stop.id) {
        spans.push(Span::styled(" ✓".to_owned(), dim));
    }
    spans.push(Span::styled(
        format!("  {}", base_name(&stop.anchor.file)),
        dim,
    ));
    if held > 1 {
        spans.push(Span::styled(format!(" · {held}"), dim));
    }
    pad_line(spans, bg, width)
}

fn sidebar_summary_line(rc: &TreeRowCtx<'_>) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        width,
        on_cursor,
        focused,
        search,
        ..
    } = rc;
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let title_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    let mut spans = vec![tree_lead(theme, 0, bg, on_cursor)];
    spans.extend(super::highlight_spans(
        "Summary",
        title_style,
        search,
        theme,
    ));
    pad_line(spans, bg, width)
}

pub(crate) fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

#[allow(clippy::too_many_lines)]
fn draw_pane(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let (theme, session, review_model, search) =
        (ctx.theme, ctx.session, ctx.review_model, ctx.search);
    let focused = diff.focus == Pane::Diff;
    let title = pane_title(&diff.source, ctx.algorithm);
    frame.render_widget(Block::new().style(Style::new().bg(theme.panel)), area);
    let [heading, inner] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(pane_heading(theme, &title, focused, theme.panel)),
        heading,
    );

    let model_base = diff.commit_model.as_ref().or(review_model);
    let model = model_base.map(|base| DiffView::rendered_model(diff.merged_model.as_ref(), base));
    let Some((model, file)) =
        model.and_then(|model| model.files.get(diff.selected).map(|file| (model, file)))
    else {
        frame.render_widget(
            Paragraph::new(Line::styled(
                " nothing to review",
                Style::new().fg(theme.dim).bg(theme.panel),
            )),
            inner,
        );
        return;
    };

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
            header_area.width,
        )),
        header_area,
    );

    let body_area = if crate::app::image::is_image(file) {
        let comment_rows = u16::try_from(diff.rows.len()).unwrap_or(u16::MAX);
        let rows_height = comment_rows.min(body_area.height / 2);
        let [picture, rest] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(rows_height)])
                .areas(body_area);
        let want = super::image_pane::draw_image_sides(
            frame,
            picture,
            theme,
            file,
            diff.image_preview.as_ref(),
        );
        diff.image_want = Some(want);
        if rows_height == 0 {
            diff.viewport = 0;
            diff.pane = rest;
            return;
        }
        rest
    } else {
        diff.image_want = None;
        body_area
    };

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
        let gutter = file_gutter_width(file);
        let highlights = diff.highlights.get(&file.path);
        let file_ctx = SplitFileCtx {
            file,
            highlights,
            gutter,
        };
        let heights: Vec<usize> = split
            .iter()
            .map(|row| split_row_height(file_ctx, row, rows_area.width))
            .collect();
        let mut starts = Vec::with_capacity(heights.len());
        let mut total = 0usize;
        for h in &heights {
            starts.push(total);
            total += h;
        }
        let (sel_start, sel_height) = match (starts.get(sel), heights.get(sel)) {
            (Some(s), Some(h)) => (*s, *h),
            _ => (0, 1),
        };
        let base = diff.scroll_align.take().map_or(diff.split_scroll, |a| {
            align_scroll(a, sel_start, sel_height, height)
        });
        let scroll = super::scroll_to_span(sel_start, sel_height, base, height, total);
        diff.split_scroll = scroll;
        diff.cursor_offset = sel_start.saturating_sub(scroll);
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
        let mut top_row = None;
        for (index, row) in split.iter().enumerate() {
            let (Some(&start), Some(&row_h)) = (starts.get(index), heights.get(index)) else {
                break;
            };
            if start + row_h <= scroll {
                continue;
            }
            if start >= scroll + height {
                break;
            }
            top_row.get_or_insert(index);
            let rendered = split_row_lines(
                ctx,
                diff,
                file_ctx,
                rows_area.width,
                row,
                RowState {
                    selected: index == sel,
                    focused,
                },
                side,
                diff.composer.as_ref(),
            );
            for (offset, line) in rendered.into_iter().enumerate() {
                let at = start + offset;
                if at < scroll || at >= scroll + height {
                    continue;
                }
                lines.push(line);
            }
        }
        let top = top_row.and_then(|from| {
            split
                .iter()
                .skip(from)
                .find_map(|r| split_right_new_no(file, r))
        });
        render_scope_crumb(frame, crumb_area, theme, diff.scopes.get(&file.path), top);
        frame.render_widget(Paragraph::new(lines), rows_area);
        return;
    }

    let height = rows_area.height.max(1) as usize;
    let cursor = diff.cursor;
    let selection = diff.selection();
    let selected = |index: usize| {
        index == cursor || selection.is_some_and(|(start, end)| index >= start && index <= end)
    };
    // wrapped rows vary in height, so we scroll in visual lines
    let rows = diff.rows().to_vec();
    let referenced = diff.referenced;
    let heights: Vec<usize> = rows
        .iter()
        .map(|row| row_height(model, row, rows_area.width))
        .collect();
    let mut starts = Vec::with_capacity(heights.len());
    let mut total = 0usize;
    for h in &heights {
        starts.push(total);
        total += h;
    }
    let (cur_start, cur_height) = match (starts.get(cursor), heights.get(cursor)) {
        (Some(s), Some(h)) => (*s, *h),
        _ => (0, 1),
    };
    let base = diff.scroll_align.take().map_or(diff.scroll, |a| {
        align_scroll(a, cur_start, cur_height, height)
    });
    let scroll = super::scroll_to_span(cur_start, cur_height, base, height, total);
    diff.scroll = scroll;
    diff.cursor_offset = cur_start.saturating_sub(scroll);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
    let mut line_rows: Vec<Option<usize>> = Vec::with_capacity(height);
    let mut top_row = None;
    for (index, row) in rows.iter().enumerate() {
        let (Some(&start), Some(&row_h)) = (starts.get(index), heights.get(index)) else {
            break;
        };
        if start + row_h <= scroll {
            continue;
        }
        if start >= scroll + height {
            break;
        }
        top_row.get_or_insert(index);
        // search ranges are keyed by row index, so an unfocused pane would show the sidebar's matches
        let ranges = search
            .filter(|_| focused)
            .map(|s| s.ranges_for(index))
            .unwrap_or_default();
        let mut marks = Mark::search(ranges);
        marks.extend(lens_marks(diff, model, row, theme));
        let mut rendered = row_lines(
            ctx,
            model,
            diff,
            row,
            rows_area.width,
            RowState {
                selected: selected(index),
                focused,
            },
            &marks,
        );
        // the cursor row keeps its own band so the reader sees where in the span they stand
        if !selected(index) {
            rendered = super::band_referenced(rendered, referenced, index, theme, rows_area.width);
        }
        for (offset, line) in rendered.into_iter().enumerate() {
            let at = start + offset;
            if at < scroll || at >= scroll + height {
                continue;
            }
            lines.push(line);
            line_rows.push(Some(index));
        }
    }
    diff.line_rows = line_rows;
    frame.render_widget(Paragraph::new(lines), rows_area);

    let top = top_row.and_then(|from| rows.iter().skip(from).find_map(|r| row_new_no(file, r)));
    render_scope_crumb(frame, crumb_area, theme, diff.scopes.get(&file.path), top);
}

fn split_row_height(file_ctx: SplitFileCtx<'_>, row: &SplitRow, width: u16) -> usize {
    let SplitRow::Pair { hunk, left, right } = *row else {
        return 1;
    };
    let Some(hunk) = file_ctx.file.hunks.get(hunk) else {
        return 1;
    };
    split_pair_height(
        left.and_then(|i| hunk.lines.get(i)),
        right.and_then(|i| hunk.lines.get(i)),
        file_ctx.gutter,
        width,
    )
}

#[allow(clippy::too_many_arguments)] // a split row's inputs, none of them a group
fn split_row_lines(
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    file_ctx: SplitFileCtx<'_>,
    width: u16,
    row: &SplitRow,
    state: RowState,
    cursor_side: Option<SplitSide>,
    composer: Option<&Composer>,
) -> Vec<Line<'static>> {
    let SplitFileCtx {
        file,
        highlights,
        gutter,
    } = file_ctx;
    match *row {
        SplitRow::Hunk { hunk } => match file.hunks.get(hunk) {
            Some(hunk) => vec![hunk_header(
                ctx.theme,
                hunk,
                width,
                state.selected,
                state.focused,
            )],
            None => vec![Line::default()],
        },
        SplitRow::Pair { hunk, left, right } => {
            let Some(hunk) = file.hunks.get(hunk) else {
                return vec![Line::default()];
            };
            let cell = |index: Option<usize>, side: SplitSide| {
                index.and_then(|i| hunk.lines.get(i)).map(|line| {
                    (
                        line,
                        highlights.and_then(|hl| split_side_syntax(hl, line, side)),
                        line_annotated(ctx.session, &file.path, line),
                    )
                })
            };
            let sel_left = state.selected && !matches!(cursor_side, Some(SplitSide::Right));
            let sel_right = state.selected && !matches!(cursor_side, Some(SplitSide::Left));
            render_split_pair(
                ctx.theme,
                cell(left, SplitSide::Left),
                cell(right, SplitSide::Right),
                gutter,
                width,
                PairSelection {
                    left: sel_left,
                    right: sel_right,
                    focused: state.focused,
                },
            )
        }
        SplitRow::Comment {
            comment,
            line,
            outdated,
        } => match ctx.session.comments.get(comment) {
            Some(found) => {
                vec![comment_row_line(
                    ctx, diff, found, line, outdated, width, state,
                )]
            }
            None => vec![Line::default()],
        },
        SplitRow::Composer { line } => match composer {
            Some(composer) => vec![composer_row_line(
                ctx.theme,
                composer,
                line,
                width,
                state.selected,
                state.focused,
            )],
            None => vec![Line::default()],
        },
        SplitRow::Fold { hunk } => {
            let label = diff
                .fold_groups
                .iter()
                .find(|group| group.hunk == hunk)
                .map(|group| group.label.clone())
                .unwrap_or_default();
            vec![fold_row(
                ctx.theme,
                &label,
                width,
                state.selected,
                state.focused,
            )]
        }
    }
}

fn split_side_syntax<'a>(
    highlights: &'a FileHighlights,
    line: &DiffLine,
    side: SplitSide,
) -> Option<&'a [StyledRange]> {
    match side {
        SplitSide::Left => syntax_row(&highlights.old, line.old_no),
        SplitSide::Right => syntax_row(&highlights.new, line.new_no),
    }
}

/// A non-default algorithm trails the title so a switch stays visible after its
/// status message clears.
fn pane_title(source: &ReviewSource, algorithm: diffler_core::diffalgo::DiffAlgorithm) -> String {
    let base = match source {
        ReviewSource::WorkingTree
        | ReviewSource::Commit { .. }
        | ReviewSource::Walkthrough { .. } => "Diff".to_owned(),
        ReviewSource::Range { oldest, newest } => {
            let short = |oid: &str| oid.get(..7).unwrap_or(oid).to_owned();
            format!("Diff {}..{}", short(oldest), short(newest))
        }
        ReviewSource::Pr { number } => format!("PR #{number}"),
        ReviewSource::Against { .. } => format!("Diff {}", source.label()),
    };
    if algorithm == diffler_core::diffalgo::DiffAlgorithm::default() {
        base
    } else {
        format!("{base} · {algorithm}")
    }
}

/// The sidebar differs from the diff pane's surface, which is what separates them with no border.
fn sidebar_bg(theme: &Theme) -> Color {
    theme.bg
}

fn pane_heading(theme: &Theme, title: &str, focused: bool, bg: Color) -> Line<'static> {
    Line::styled(
        format!(" {title}"),
        Style::new()
            .fg(if focused { theme.accent } else { theme.dim })
            .bg(bg),
    )
}

fn open_comment_count(session: &Session, path: &str) -> usize {
    session
        .comments
        .iter()
        .filter(|c| c.anchor.file == path && c.status != CommentStatus::Resolved)
        .count()
}

/// Each sidebar group's `(added, deleted)` totals, from one pass over the model
/// per frame, since a header row knows its name and not its members.
#[derive(Default)]
struct GroupStat {
    dirs: HashMap<String, (usize, usize)>,
    sections: HashMap<Bucket, (usize, usize)>,
}

impl GroupStat {
    fn collect(diff: &DiffView, model: &DiffModel, session: &Session) -> Self {
        let mut stat = Self::default();
        for file in &model.files {
            let (added, deleted) = file.diffstat();
            let viewed = session.is_viewed(&file.path, &file.content_hash());
            let tally = |slot: &mut (usize, usize)| {
                slot.0 += added;
                slot.1 += deleted;
            };
            match diff.layout {
                FileLayout::Kinds => {
                    let bucket = Bucket::Kind(diff.kind_of(&file.path));
                    tally(stat.sections.entry(bucket).or_default());
                }
                FileLayout::Review => {
                    let bucket = if viewed {
                        Bucket::Viewed
                    } else {
                        Bucket::ToReview
                    };
                    tally(stat.sections.entry(bucket).or_default());
                }
                FileLayout::Walkthrough => {}
                FileLayout::Tree | FileLayout::List => {
                    for (at, _) in file.path.match_indices('/') {
                        let dir = &file.path[..at];
                        tally(stat.dirs.entry(dir.to_owned()).or_default());
                    }
                }
            }
        }
        stat
    }
}

/// Right-align `tail` when it fits; a row too narrow drops it and keeps its name.
fn push_right(spans: &mut Vec<Span<'static>>, tail: Vec<Span<'static>>, width: u16, bg: Color) {
    if tail.is_empty() {
        return;
    }
    let tail_width: usize = tail.iter().map(Span::width).sum();
    let used: usize = spans.iter().map(Span::width).sum();
    let Some(pad) = super::right_align_pad(used, tail_width, width as usize) else {
        return;
    };
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::new().bg(bg)));
    }
    spans.extend(tail);
}

fn sidebar_row_bg(theme: &Theme, on_cursor: bool, focused: bool) -> Color {
    if on_cursor {
        cursor_band(theme, sidebar_bg(theme), focused)
    } else {
        sidebar_bg(theme)
    }
}

/// The cursor `▌` marker plus the tree indent for `depth`.
fn tree_lead(theme: &Theme, depth: usize, bg: Color, on_cursor: bool) -> Span<'static> {
    // some themes tint the cursor band faintly, so we add a bar
    let marker = if on_cursor { "▌" } else { " " };
    Span::styled(
        format!("{marker}{}", " ".repeat(depth * 2)),
        Style::new().fg(theme.accent).bg(bg),
    )
}

fn sidebar_dir_line(
    rc: &TreeRowCtx<'_>,
    name: &str,
    folded: bool,
    stat: (usize, usize),
) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        depth,
        width,
        on_cursor,
        focused,
        search,
    } = rc;
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let arrow = if folded { "▸ " } else { "▾ " };
    let name_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    let mut spans = vec![
        tree_lead(theme, depth, bg, on_cursor),
        Span::styled(arrow.to_owned(), Style::new().fg(theme.dim).bg(bg)),
    ];
    spans.extend(super::highlight_spans(name, name_style, search, theme));
    let tail = diffstat_spans(theme, stat.0, stat.1, bg);
    push_right(&mut spans, tail, width, bg);
    pad_line(spans, bg, width)
}

fn sidebar_section_line(
    rc: &TreeRowCtx<'_>,
    bucket: Bucket,
    count: usize,
    stat: (usize, usize),
    folded: bool,
) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        width,
        on_cursor,
        focused,
        ..
    } = rc;
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let tail = diffstat_spans(theme, stat.0, stat.1, bg);
    group_header_line(
        HeaderCtx {
            theme,
            bg,
            width,
            on_cursor,
        },
        bucket.label(),
        count,
        folded,
        tail,
    )
}

fn sidebar_file_line(
    rc: &TreeRowCtx<'_>,
    file: &FileDiff,
    name: &str,
    viewed: bool,
    open: usize,
) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        depth,
        width,
        on_cursor,
        focused,
        search,
    } = rc;
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let (glyph, glyph_colour) = if viewed {
        ('✓', theme.added)
    } else {
        (file.status.glyph(), status_color(theme, file.status))
    };
    let mut spans = vec![
        tree_lead(theme, depth, bg, on_cursor),
        Span::styled(format!("{glyph} "), Style::new().fg(glyph_colour).bg(bg)),
    ];
    // " ·{open}" is 2 + the count's digits wide
    let suffix_width = if open > 0 {
        2 + open.to_string().len()
    } else {
        0
    };
    let used = spans.iter().map(Span::width).sum::<usize>() + suffix_width;
    let room = (width as usize).saturating_sub(used + 1);
    let name_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    // we highlight before clipping so a match stays lit; a path front-elides to keep its basename
    let parent = name.rfind('/').map_or(0, |at| at + 1);
    let highlighted = super::highlight_spans_split(name, parent, dim, name_style, search, theme);
    spans.extend(clip_spans(
        highlighted,
        room,
        name.contains('/'),
        name_style,
    ));
    if open > 0 {
        spans.push(Span::styled(format!(" ·{open}"), dim));
    }
    let (added, deleted) = file.diffstat();
    push_right(
        &mut spans,
        diffstat_spans(theme, added, deleted, bg),
        width,
        bg,
    );
    pad_line(spans, bg, width)
}

/// Clip styled `spans` to `room` chars, keeping each span's style. `front`
/// elides from the left, otherwise from the right.
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

fn row_height(model: &DiffModel, row: &DiffRow, width: u16) -> usize {
    let DiffRow::Line { file, hunk, line } = row else {
        return 1;
    };
    model
        .files
        .get(*file)
        .and_then(|f| {
            let line = f.hunks.get(*hunk)?.lines.get(*line)?;
            Some(diff_line_height(line, file_gutter_width(f), width))
        })
        .unwrap_or(1)
}

fn row_lines(
    ctx: &RenderCtx<'_>,
    model: &DiffModel,
    diff: &DiffView,
    row: &DiffRow,
    width: u16,
    state: RowState,
    marks: &[(std::ops::Range<usize>, Mark)],
) -> Vec<Line<'static>> {
    let highlights = &diff.highlights;
    match row {
        DiffRow::Hunk { file, hunk } => {
            match model.files.get(*file).and_then(|f| f.hunks.get(*hunk)) {
                Some(hunk) => vec![hunk_header(
                    ctx.theme,
                    hunk,
                    width,
                    state.selected,
                    state.focused,
                )],
                None => vec![Line::default()],
            }
        }
        DiffRow::Line { file, hunk, line } => {
            let Some(file) = model.files.get(*file) else {
                return vec![Line::default()];
            };
            let Some(line) = file.hunks.get(*hunk).and_then(|h| h.lines.get(*line)) else {
                return vec![Line::default()];
            };
            let syntax = highlights
                .get(&file.path)
                .and_then(|cached| line_syntax(&cached.old, &cached.new, line));
            let annotated = line_annotated(ctx.session, &file.path, line);
            render_diff_line(
                ctx.theme,
                line,
                syntax,
                file_gutter_width(file),
                width,
                LineFlags {
                    selected: state.selected,
                    focused: state.focused,
                    annotated,
                },
                marks,
            )
        }
        DiffRow::Comment {
            comment,
            line,
            outdated,
        } => match ctx.session.comments.get(*comment) {
            Some(found) => {
                vec![comment_row_line(
                    ctx, diff, found, *line, *outdated, width, state,
                )]
            }
            None => vec![Line::default()],
        },
        DiffRow::Composer { line } => match diff.composer.as_ref() {
            Some(composer) => vec![composer_row_line(
                ctx.theme,
                composer,
                *line,
                width,
                state.selected,
                state.focused,
            )],
            None => vec![Line::default()],
        },
        DiffRow::Summary { line } => match diff.active_walkthrough(ctx.session) {
            Some(walkthrough) => vec![summary_row_line(
                ctx,
                diff,
                walkthrough,
                *line,
                width,
                state,
            )],
            None => vec![Line::default()],
        },
        DiffRow::Fold { group, .. } => {
            let label = diff
                .fold_groups
                .get(*group)
                .map_or_else(String::new, |g| g.label.clone());
            vec![fold_row(
                ctx.theme,
                &label,
                width,
                state.selected,
                state.focused,
            )]
        }
    }
}

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

fn line_annotated(session: &Session, file_path: &str, line: &DiffLine) -> bool {
    session.comments.iter().any(|c| {
        if c.anchor.file != file_path {
            return false;
        }
        let Some((start, end)) = c.anchor.span() else {
            return false;
        };
        let no = if c.anchor.on_old_side {
            line.old_no
        } else {
            line.new_no
        };
        no.is_some_and(|n| start <= n && n <= end)
    })
}

fn row_new_no(file: &FileDiff, row: &DiffRow) -> Option<u32> {
    match *row {
        DiffRow::Line { hunk, line, .. } => file.hunks.get(hunk)?.lines.get(line)?.new_no,
        _ => None,
    }
}

fn split_right_new_no(file: &FileDiff, row: &SplitRow) -> Option<u32> {
    match *row {
        SplitRow::Pair { hunk, right, .. } => file.hunks.get(hunk)?.lines.get(right?)?.new_no,
        _ => None,
    }
}

fn pane_header_line(
    theme: &Theme,
    file: &FileDiff,
    viewed: bool,
    // (open or replied, total)
    comments: (usize, usize),
    width: u16,
) -> Line<'static> {
    let bg = theme.panel;
    let dim = Style::new().fg(theme.dim).bg(bg);
    let mode = file.status.label();
    let mut spans = vec![Span::styled(format!(" {mode:<10}"), dim)];
    spans.push(Span::styled(
        file.path.clone(),
        Style::new().fg(theme.accent).bg(bg),
    ));
    if file.binary {
        spans.push(Span::styled(" (binary)".to_owned(), dim));
    }
    if viewed {
        spans.push(Span::styled(" ✓ viewed".to_owned(), dim));
    }
    let (open, total) = comments;
    if open > 0 {
        let noun = if open == 1 { "comment" } else { "comments" };
        spans.push(Span::styled(format!(" · {open} {noun}"), dim));
    } else if total > 0 {
        spans.push(Span::styled(" · resolved".to_owned(), dim));
    }
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

/// Replies take the comments sidebar's author colours, and every reply but the
/// reader's own indents into the other lane.
#[derive(Clone, Copy)]
struct ThreadLook<'a> {
    theme: &'a Theme,
    bg: Color,
    human: &'a str,
    orders: &'a HashMap<&'a str, usize>,
}

impl ThreadLook<'_> {
    fn color(&self, author: &str) -> Color {
        let order = self.orders.get(author).copied().unwrap_or(0);
        author_color(self.theme, self.bg, self.human, author, order)
    }

    fn lane(&self, bar: Span<'static>, author: &str) -> Vec<Span<'static>> {
        let indent = if !self.human.is_empty() && author == self.human {
            0
        } else {
            crate::app::REPLY_LANE
        };
        vec![
            bar,
            Span::styled(" ".repeat(indent), Style::new().bg(self.bg)),
            Span::styled(
                "▌ ".to_owned(),
                Style::new().fg(self.color(author)).bg(self.bg),
            ),
        ]
    }

    fn reply_spans(&self, part: &CommentLine, bar: Span<'static>) -> Vec<Span<'static>> {
        let Self { theme, bg, .. } = *self;
        let dim = Style::new().fg(theme.dim).bg(bg);
        let fg = Style::new().fg(theme.fg).bg(bg);
        match part {
            CommentLine::ReplyHead { author } => {
                let mut spans = self.lane(bar, author);
                spans.push(Span::styled(
                    author.clone(),
                    Style::new()
                        .fg(self.color(author))
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ));
                spans
            }
            CommentLine::Reply {
                author,
                spans: runs,
            } => {
                let mut spans = self.lane(bar, author);
                spans.extend(runs.iter().map(|run| md_span(run, fg, theme)));
                spans
            }
            CommentLine::FoldedReplies { count, authors } => {
                let mut spans = vec![
                    bar,
                    Span::styled("  ".to_owned(), Style::new().bg(bg)),
                    Span::styled(
                        crate::app::folded_replies_text(*count),
                        Style::new().fg(theme.accent).bg(bg),
                    ),
                    Span::styled(" · ".to_owned(), dim),
                ];
                for (at, author) in authors.iter().enumerate() {
                    if at > 0 {
                        spans.push(Span::styled(", ".to_owned(), dim));
                    }
                    spans.push(Span::styled(
                        author.clone(),
                        Style::new().fg(self.color(author)).bg(bg),
                    ));
                }
                spans
            }
            _ => vec![bar],
        }
    }
}

fn comment_row_line(
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    comment: &Comment,
    line: usize,
    outdated: bool,
    width: u16,
    state: RowState,
) -> Line<'static> {
    let theme = ctx.theme;
    let (status_label, accent) = match comment.status {
        CommentStatus::Open => ("open", theme.warn_fg),
        CommentStatus::Replied => ("replied", theme.accent),
        CommentStatus::Resolved => ("resolved", theme.dim),
    };
    let (bg, bar) = card_frame(theme, state.selected, state.focused, accent);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let unresolved = diff.unresolved_anchors.get(&comment.id).copied();
    let lines = diff
        .card_views()
        .lines_with(comment, width, Some(ctx.highlighter));
    let Some(part) = lines.get(line) else {
        return Line::default();
    };
    let thread = ThreadLook {
        theme,
        bg,
        human: ctx.human_author,
        orders: &ctx.author_orders,
    };
    let spans = match part {
        CommentLine::Header => {
            let mut spans = vec![bar];
            if let Some(title) = comment.title.as_ref() {
                spans.push(Span::styled(
                    format!("{title}  "),
                    Style::new().fg(theme.accent).bg(bg),
                ));
            }
            spans.extend([
                Span::styled(
                    comment.author.clone(),
                    Style::new().fg(thread.color(&comment.author)).bg(bg),
                ),
                Span::styled(" · ".to_owned(), dim),
                Span::styled(status_label.to_owned(), Style::new().fg(accent).bg(bg)),
            ]);
            if outdated {
                spans.push(Span::styled(
                    " · outdated".to_owned(),
                    Style::new().fg(theme.warn_fg).bg(bg),
                ));
            }
            // only the anchor worker can tell a lost anchor from an unread one, so we read its map
            if unresolved.is_some() {
                spans.push(Span::styled(
                    " · stale".to_owned(),
                    Style::new().fg(theme.warn_fg).bg(bg),
                ));
            }
            spans
        }
        CommentLine::Body(runs) => {
            let mut spans = vec![bar];
            spans.extend(runs.iter().map(|run| md_span(run, fg, theme)));
            spans
        }
        CommentLine::Note(runs) => {
            let mut spans = vec![bar];
            spans.extend(runs.iter().map(|run| md_span(run, dim, theme)));
            spans
        }
        CommentLine::Figure { block, row } => {
            let Some(drawn) = ctx
                .rasters
                .get(&(comment.id.clone(), *block))
                .and_then(|lines| lines.get(*row))
            else {
                return Line::default();
            };
            return super::fill_row(drawn.clone(), bg, width);
        }
        CommentLine::ReplyGap
        | CommentLine::ReplyHead { .. }
        | CommentLine::Reply { .. }
        | CommentLine::FoldedReplies { .. } => {
            // each reply draws its own bar, so we leave the card's bar column blank
            let margin = Span::styled("  ".to_owned(), Style::new().bg(bg));
            thread.reply_spans(part, margin)
        }
        CommentLine::Footer => vec![Span::styled(
            "  ▌".to_owned(),
            Style::new().fg(accent).bg(bg),
        )],
    };
    pad_line(spans, bg, width)
}

/// The walkthrough summary's card: a "Summary" header with no status or author,
/// since nothing threads on it.
fn summary_row_line(
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    walkthrough: &diffler_core::walkthrough::Walkthrough,
    line: usize,
    width: u16,
    state: RowState,
) -> Line<'static> {
    let theme = ctx.theme;
    let (bg, bar) = card_frame(theme, state.selected, state.focused, theme.accent);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let key = summary_figure_key(&walkthrough.id);
    let blocks = crate::app::blocks_of(&diff.figures, &key);
    let summary = walkthrough.summary.as_deref().unwrap_or_default();
    let lines = summary_display(summary, width, Some(ctx.highlighter), blocks);
    let Some(part) = lines.get(line) else {
        return Line::default();
    };
    let spans = match part {
        CommentLine::Header => vec![
            bar,
            Span::styled("Summary".to_owned(), Style::new().fg(theme.accent).bg(bg)),
        ],
        CommentLine::Body(runs) => {
            let mut spans = vec![bar];
            spans.extend(runs.iter().map(|run| md_span(run, fg, theme)));
            spans
        }
        CommentLine::Figure { block, row } => {
            let Some(drawn) = ctx
                .rasters
                .get(&(key.clone(), *block))
                .and_then(|lines| lines.get(*row))
            else {
                return Line::default();
            };
            return super::fill_row(drawn.clone(), bg, width);
        }
        // the summary has no anchor and no thread
        CommentLine::Note(_)
        | CommentLine::ReplyGap
        | CommentLine::ReplyHead { .. }
        | CommentLine::Reply { .. }
        | CommentLine::FoldedReplies { .. } => return Line::default(),
        CommentLine::Footer => vec![Span::styled(
            "  ▌".to_owned(),
            Style::new().fg(theme.accent).bg(bg),
        )],
    };
    pad_line(spans, bg, width)
}

/// The open composer, drawn as the card it will become, with the caret as a reversed cell.
fn composer_row_line(
    theme: &Theme,
    composer: &Composer,
    line: usize,
    width: u16,
    selected: bool,
    focused: bool,
) -> Line<'static> {
    let accent = theme.accent;
    let (bg, bar) = card_frame(theme, selected, focused, accent);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let lines = composer.display(width);
    let Some(part) = lines.get(line) else {
        return Line::default();
    };
    let spans = match part {
        ComposerLine::Header => vec![
            bar,
            Span::styled(composer_title(composer), Style::new().fg(accent).bg(bg)),
        ],
        ComposerLine::Body { text, cursor } => {
            let mut spans = vec![bar];
            let Some(column) = *cursor else {
                spans.push(Span::styled(text.clone(), fg));
                return pad_line(spans, bg, width);
            };
            let head: String = text.chars().take(column).collect();
            let under = text.chars().nth(column).unwrap_or(' ');
            let tail: String = text.chars().skip(column + 1).collect();
            spans.push(Span::styled(head, fg));
            spans.push(Span::styled(
                under.to_string(),
                fg.add_modifier(Modifier::REVERSED),
            ));
            spans.push(Span::styled(tail, fg));
            spans
        }
        ComposerLine::Footer => vec![
            Span::styled("  ▌ ".to_owned(), Style::new().fg(accent).bg(bg)),
            Span::styled("enter".to_owned(), fg),
            Span::styled(" submit · ".to_owned(), dim),
            Span::styled("a-enter".to_owned(), fg),
            Span::styled(" newline · ".to_owned(), dim),
            Span::styled("esc".to_owned(), fg),
            Span::styled(" cancel".to_owned(), dim),
        ],
    };
    pad_line(spans, bg, width)
}

fn composer_title(composer: &Composer) -> String {
    match &composer.kind {
        ComposerKind::New { anchor } => match (anchor.line, anchor.line_end) {
            (Some(line), Some(end)) => format!("comment on {}:{line}-{end}", anchor.file),
            (Some(line), None) => format!("comment on {}:{line}", anchor.file),
            _ => format!("comment on {}", anchor.file),
        },
        ComposerKind::Reply { .. } => "reply".to_owned(),
        ComposerKind::Edit { .. } => "edit comment".to_owned(),
    }
}

/// Recolouring flags (code, link, muted) win over `base`'s foreground.
pub(super) fn md_span(run: &MdSpan, base: Style, theme: &Theme) -> Span<'static> {
    let mut style = base;
    if run.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if run.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if run.strike {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if run.code {
        style = style.fg(theme.accent);
    }
    if run.link {
        style = style.fg(theme.accent).add_modifier(Modifier::UNDERLINED);
    }
    if run.muted {
        style = style.fg(theme.dim);
    }
    if let Some((r, g, b)) = run.fg {
        style = style.fg(Color::Rgb(r, g, b));
    }
    Span::styled(run.text.clone(), style)
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
    use crate::app::rowsel::RowSelect;
    use ratatui::Terminal;
    use ratatui::text::Line;

    use super::{BlockLook, LineKind, RefEntry, reference_block, reference_header};

    #[test]
    fn renders_the_comments_sidebar() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('l'));
        app.handle(key('j'));
        app.handle(key('c'));
        for c in "this line reads oddly and should wrap across the column".chars() {
            app.handle(key(c));
        }
        app.handle(crate::test_support::code_key(
            crossterm::event::KeyCode::Enter,
        ));
        app.handle(key('C'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_reference_preview_cuts_its_shared_indent() {
        let theme = Theme::github_dark();
        let preview = |text: &str, number| crate::app::PreviewLine {
            kind: LineKind::Added,
            number: Some(number),
            text: text.to_owned(),
        };
        let entry = RefEntry {
            path: "src/a.rs".to_owned(),
            on_old_side: false,
            line: 2,
            range: 12..17,
            preview: vec![
                preview("        if ok {", 1),
                preview("            total(1)", 2),
            ],
            own: 1,
            group_len: Some(1),
        };
        let look = BlockLook {
            theme: &theme,
            surface: theme.bg,
            selected: false,
            focused: false,
            color: theme.accent,
            width: 40,
            syntax: None,
        };
        let lines = reference_block(look, &entry);
        assert!(
            line_text(&lines[0]).contains("+ if ok {"),
            "{:?}",
            line_text(&lines[0])
        );
        assert!(line_text(&lines[1]).contains("+     total(1)"));
    }

    #[test]
    fn a_reference_header_keeps_the_file_name_when_the_path_is_long() {
        let theme = Theme::github_dark();
        let path = "tests/app/routers/test_zoning_plan_search.py";
        let text = line_text(&reference_header(&theme, theme.bg, 38, path, 3));
        assert!(text.contains("…"), "{text}");
        assert!(text.contains("test_zoning_plan_search.py (3)"), "{text}");
    }

    #[test]
    fn reply_authors_step_after_comment_authors() {
        let mut session = diffler_core::session::Session::default();
        let anchor = diffler_core::session::Anchor {
            file: "a.rs".to_owned(),
            line: Some(1),
            line_end: None,
            on_old_side: false,
            line_text: None,
        };
        let id = session
            .add_comment(anchor.clone(), "alice", "one")
            .id
            .clone();
        session.add_comment(anchor, "bob", "two");
        session.reply(&id, "carol", "three");
        session.reply(&id, "agent", "four");
        let orders = super::pane_author_orders(&session, None, "reviewer");
        assert_eq!(orders.get("alice"), Some(&0));
        assert_eq!(orders.get("bob"), Some(&1));
        assert_eq!(orders.get("carol"), Some(&2));
        assert_eq!(
            orders.get("agent"),
            None,
            "the agent keeps its fixed colour"
        );
    }

    #[test]
    fn author_color_is_fixed_for_human_and_agent_regardless_of_order() {
        let theme = Theme::github_dark();
        let bg = theme.bg;
        assert_eq!(
            super::author_color(&theme, bg, "reviewer", "reviewer", 3),
            theme.accent,
            "the reviewer's own comments take the fixed accent colour"
        );
        assert_eq!(
            super::author_color(&theme, bg, "reviewer", crate::mcp::AGENT_AUTHOR, 7),
            theme.purple,
            "the agent's comments take the fixed purple colour"
        );
        assert_eq!(
            super::author_color(&theme, bg, "reviewer", "alice", 0),
            super::author_color(&theme, bg, "reviewer", "alice", 0),
            "the same order always reads the same colour"
        );
    }

    #[test]
    fn several_authors_get_distinct_hues_and_never_the_fixed_two() {
        let theme = Theme::github_dark();
        let bg = theme.bg;
        let authors = ["alice", "bob", "carol", "dave", "erin", "frank"];
        let orders = super::author_orders(authors.into_iter(), "reviewer");
        let colours: Vec<ratatui::style::Color> = authors
            .iter()
            .map(|author| {
                let order = *orders.get(author).expect("every author was seeded");
                super::author_color(&theme, bg, "reviewer", author, order)
            })
            .collect();
        for (i, colour) in colours.iter().enumerate() {
            assert_ne!(
                *colour, theme.accent,
                "{}: never the human's colour",
                authors[i]
            );
            assert_ne!(
                *colour, theme.purple,
                "{}: never the agent's colour",
                authors[i]
            );
            for (j, other) in colours.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    colour, other,
                    "{} and {} collide: {colours:?}",
                    authors[i], authors[j]
                );
            }
        }
    }

    /// A stop's body stays in its card; ten stops of four bullets would bury the list.
    #[test]
    fn the_comments_sidebar_lists_a_titled_comment_by_its_title_alone() {
        let (_fixture, mut app) = diff_app();
        let source = app.active_review_source();
        let file = app
            .diff
            .as_ref()
            .and_then(|diff| {
                diff.model(&app.review)
                    .files
                    .first()
                    .map(|f| f.path.clone())
            })
            .expect("a file in the diff");
        let anchor = diffler_core::session::Anchor {
            file,
            line: None,
            line_end: None,
            on_old_side: false,
            line_text: None,
        };
        let id = app
            .review
            .session_for_mut(&source)
            .add_comment(
                anchor,
                "agent",
                "We refuse a default here because it hides a missing list.",
            )
            .id
            .clone();
        if let Some(comment) = app
            .review
            .session_for_mut(&source)
            .comments
            .iter_mut()
            .find(|comment| comment.id == id)
        {
            comment.title = Some("Missing staff list stops the run".to_owned());
        }
        app.handle(key('C'));
        let screen = render(&mut app).backend().to_string();
        // we slice by column since box-drawing glyphs make a byte slice land mid-character
        let pane_start = screen
            .lines()
            .find_map(|row| row.find("Comments (").map(|at| row[..at].chars().count()))
            .expect("the comments pane heading");
        let pane: String = screen
            .lines()
            .map(|row| row.chars().skip(pane_start).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(pane.contains("Missing staff list"), "{pane}");
        assert!(
            !pane.contains("We refuse a default here"),
            "the body belongs in the card, not the list: {pane}"
        );
    }

    #[test]
    fn the_comments_sidebar_opens_empty_on_a_review_without_comments() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('C'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    fn app_with_grouped_comments() -> (crate::test_support::Fixture, App) {
        let (fixture, mut app) = diff_app();
        let source = app.active_review_source();
        app.review.session_for_mut(&source).add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "why 42?",
        );
        app.review.session_for_mut(&source).add_comment(
            diffler_core::session::Anchor {
                file: "todo.md".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "alice",
            "needs a date",
        );
        let resolved = app
            .review
            .session_for_mut(&source)
            .add_comment(
                diffler_core::session::Anchor {
                    file: "todo.md".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "alice",
                "already fixed",
            )
            .id
            .clone();
        app.review.session_for_mut(&source).resolve(&resolved);
        (fixture, app)
    }

    #[test]
    fn comments_pane_flat_lists_every_comment_with_no_headers() {
        let (_fixture, mut app) = app_with_grouped_comments();
        app.handle(key('C'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comments_pane_grouped_by_file_renders_a_header_per_file() {
        let (_fixture, mut app) = app_with_grouped_comments();
        app.handle(key('C'));
        app.handle(key('t')); // flat -> file
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comments_pane_grouped_by_author_renders_a_header_per_author() {
        let (_fixture, mut app) = app_with_grouped_comments();
        app.handle(key('C'));
        app.handle(key('t')); // file
        app.handle(key('t')); // author
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn comments_pane_grouped_by_status_starts_resolved_folded() {
        let (_fixture, mut app) = app_with_grouped_comments();
        app.handle(key('C'));
        app.handle(key('t')); // file
        app.handle(key('t')); // author
        app.handle(key('t')); // status
        insta::assert_snapshot!(render(&mut app).backend());
    }

    fn busy_anchor(file: &str, line: u32) -> diffler_core::session::Anchor {
        diffler_core::session::Anchor {
            file: file.to_owned(),
            line: Some(line),
            line_end: None,
            on_old_side: false,
            line_text: None,
        }
    }

    #[test]
    fn a_busy_comments_pane_stays_dense_with_a_long_name_an_elided_body_and_two_folds() {
        let (_fixture, mut app) = diff_app();
        let source = app.active_review_source();
        let reviewer_id = app
            .review
            .session_for_mut(&source)
            .add_comment(busy_anchor("src/lib.rs", 1), "reviewer", "why 42?")
            .id
            .clone();
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("src/lib.rs", 2),
            "alexandra-the-longform-reviewer",
            "looks fine",
        );
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("src/lib.rs", 3),
            "dave",
            "This preview has to run long enough that the collapsed row has \
             no choice but to elide it with an ellipsis at the end.",
        );
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("ci.yml", 1),
            "bob",
            "looks fine",
        );
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("ci.yml", 1),
            "carol",
            "ship it",
        );
        app.review.session_for_mut(&source).add_comment(
            busy_anchor("todo.md", 1),
            "agent",
            "flagged for follow-up",
        );
        app.handle(key('C'));
        app.handle(key('t')); // flat -> file
        let diff = app.diff.as_mut().expect("diff");
        diff.comment_folds.insert("file:ci.yml".to_owned());
        diff.comment_folds.insert("file:todo.md".to_owned());
        let open_row = app
            .comment_rows()
            .iter()
            .position(
                |row| matches!(row, super::CommentPaneRow::Item { id, .. } if *id == reviewer_id),
            )
            .expect("the reviewer's comment has a row under src/lib.rs");
        app.comments_to(open_row);
        insta::assert_snapshot!(render(&mut app).backend());
    }
    use ratatui::backend::TestBackend;

    use ratatui::style::Style;

    use super::clip_spans;
    use crate::app::{App, DiffRow, Pane};
    use crate::config::LoadedConfig;
    use crate::test_support::{
        Fixture, key, mouse_click, mouse_drag, mouse_right_click, mouse_scroll, render,
        settle_submit, standard_fixture,
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

    fn diff_app() -> (crate::test_support::Fixture, App) {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        (fixture, app)
    }

    #[test]
    fn diff_algorithm_picker_switches_the_algorithm_live() {
        let (_fixture, mut app) = diff_app();
        app.handle(crate::test_support::ctrl_key('a'));
        insta::assert_snapshot!(render(&mut app).backend());

        // myers, minimal, patience, histogram: three steps down
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(crate::test_support::code_key(
            crossterm::event::KeyCode::Enter,
        ));
        assert_eq!(
            app.config.diff.algorithm,
            diffler_core::diffalgo::DiffAlgorithm::Histogram
        );
        let screen = render(&mut app).backend().to_string();
        assert!(
            screen.contains("histogram"),
            "pane heading names the active algorithm: {screen}"
        );
    }

    /// The content hash is the same under both algorithms, so an enrichment
    /// queued before the switch could put the old hunks back.
    #[test]
    fn a_switch_rediffs_everything_and_outlives_an_enrichment_in_flight() {
        use diffler_core::diffalgo::{DiffAlgorithm, histogram_hunks};
        let old = "begin\nrepeat\nrepeat\nunique_anchor\nrepeat\nrepeat\nend\n";
        let new = "begin\nunique_anchor\nrepeat\nrepeat\nrepeat\nrepeat\nend\n";
        let fixture = crate::test_support::Fixture::new();
        fixture.write("a.txt", old);
        fixture.commit_all("base");
        fixture.write("a.txt", new);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        app.queue_enrich_selected();

        app.apply_diff_algorithm("histogram");
        app.settle_rediff();
        let expected = histogram_hunks(old, new, "a.txt", 3, true);
        let ids = |hunks: &[diffler_core::model::Hunk]| -> Vec<_> {
            hunks.iter().map(|h| h.id.clone()).collect()
        };
        app.enrich_now();
        assert_eq!(
            ids(&app.review.model().files[0].hunks),
            ids(&expected),
            "the stale job left the histogram hunks alone"
        );
        app.queue_enrich_selected();
        app.enrich_now();
        assert_eq!(ids(&app.review.model().files[0].hunks), ids(&expected));
        assert_eq!(
            ids(&app.review.status.unstaged.files[0].hunks),
            ids(&expected),
            "the status sections re-diffed too"
        );
        assert_eq!(app.config.diff.algorithm, DiffAlgorithm::Histogram);
    }

    #[test]
    fn a_switch_says_whether_the_hunks_changed() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write(
            "a.txt",
            "begin\nrepeat\nrepeat\nunique_anchor\nrepeat\nrepeat\nend\n",
        );
        fixture.write("b.txt", "one\ntwo\nthree\n");
        fixture.commit_all("base");
        fixture.write(
            "a.txt",
            "begin\nunique_anchor\nrepeat\nrepeat\nrepeat\nrepeat\nend\n",
        );
        fixture.write("b.txt", "one\nTWO\nthree\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let said = |app: &App| app.message.as_ref().map(|m| m.text.clone());

        app.apply_diff_algorithm("patience");
        app.settle_rediff();
        assert_eq!(
            said(&app).as_deref(),
            Some("patience changed the hunks of 1 file")
        );

        app.apply_diff_algorithm("patience");
        app.settle_rediff();
        assert_eq!(
            said(&app).as_deref(),
            Some("patience gives the same hunks here")
        );
    }

    #[test]
    fn a_rediff_keeps_the_cursor_moved_while_it_ran() {
        use crate::app::rowsel::RowText as _;
        use std::fmt::Write as _;
        let fixture = Fixture::new();
        let mut base = String::new();
        for i in 1..=40 {
            let _ = writeln!(base, "line {i}");
        }
        fixture.write("a.txt", &base);
        fixture.commit_all("base");
        let edited = base
            .replace("line 5\n", "LINE FIVE\n")
            .replace("line 35\n", "LINE THIRTY-FIVE\n");
        fixture.write("a.txt", &edited);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("a.txt");
        let _ = render(&mut app);

        app.apply_diff_algorithm("histogram");
        let diff = app.diff.as_mut().unwrap();
        let target = (0..diff.rows().len())
            .find(|&row| diff.row_text(row) == " line 36")
            .unwrap();
        diff.cursor = target;
        app.settle_rediff();

        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.row_text(diff.cursor), " line 36");
    }

    #[test]
    fn a_view_opened_during_a_rediff_rebuilds_when_it_lands() {
        let old = "begin\nrepeat\nrepeat\nunique_anchor\nrepeat\nrepeat\nend\n";
        let new = "begin\nunique_anchor\nrepeat\nrepeat\nrepeat\nrepeat\nend\n";
        let fixture = Fixture::new();
        fixture.write("a.txt", old);
        fixture.commit_all("base");
        fixture.write("a.txt", new);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let rows = |app: &mut App| {
            use crate::app::rowsel::RowText as _;
            let _ = render(app);
            let diff = app.diff.as_ref().unwrap();
            (0..diff.rows().len())
                .map(|row| diff.row_text(row))
                .collect::<Vec<_>>()
        };

        let _ = app.review.model();
        app.apply_diff_algorithm("histogram");
        app.open_working_tree_diff(None);
        let stale = rows(&mut app);
        app.settle_rediff();
        let landed = rows(&mut app);
        app.diff = None;
        app.open_working_tree_diff(None);
        let fresh = rows(&mut app);
        assert_ne!(stale, fresh, "the two algorithms draw different rows");
        assert_eq!(landed, fresh);
    }

    #[test]
    fn the_walkthrough_layout_names_itself_in_the_sidebar_heading() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        let screen = render(&mut app).backend().to_string();
        assert!(screen.contains("How the answer moved"), "{screen}");
        assert!(!screen.contains(" Files"), "{screen}");
    }

    #[test]
    fn a_broken_pin_shows_in_the_sidebar_heading() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        let source = crate::test_support::seat_walkthrough(
            &mut app,
            "tour",
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        app.review
            .session_for_mut(&source)
            .walkthrough
            .as_mut()
            .expect("walkthrough")
            .rev = Some("0000000000000000000000000000000000dead".to_owned());
        app.open_walkthrough_diff("w1");
        let request = app
            .pending_walkthrough
            .take()
            .expect("resolution queued for the walkthrough");
        let root = app.review.repo_root.clone();
        let read = diffler_core::review::Review::compute_walkthrough_files(
            &root,
            request.read_rev.as_deref(),
            request.read_first,
            &request.files,
        );
        assert!(read.pin_broken, "the garbage rev must not resolve");
        app.handle(crate::event::AppEvent::WalkthroughAnchors {
            contents: read.contents,
            pin_broken: read.pin_broken,
            token: request.token,
        });

        let screen = render(&mut app).backend().to_string();
        assert!(screen.contains("pin lost"), "{screen}");
    }

    const STOP_BODY: &str = "\
The layer that wins is the last one to **set** the key.

| Layer | Wins on |
| --- | --- |
| built-in | nothing |
| cli | every key |

```mermaid
flowchart LR
  load[load defaults] --> merge[merge]
  click merge \"src/lib.rs#answer\"
```
";

    #[test]
    fn a_stop_card_renders_its_body_table_and_figure_under_the_span() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[
                ("What changed", None, "the overview, pointing at nothing"),
                ("The answer", Some("src/lib.rs#answer"), STOP_BODY),
            ],
        );
        app.handle(key('j'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn the_current_anchorless_stop_shows_its_card_alone() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[
                ("What changed", None, "the overview, pointing at nothing"),
                ("The answer", Some("src/lib.rs#answer"), "why 42"),
            ],
        );
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_stop_in_a_large_file_windows_to_its_span() {
        let fixture = crate::test_support::big_file_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The change", Some("big.txt:100"), "why line 100 changed")],
        );
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// A slide shows only its region, so banding it would paint every code row.
    #[test]
    fn a_slide_never_bands_its_whole_region_while_a_file_layout_bands_a_span() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        let band = crate::theme::blend(app.theme.bg, app.theme.accent, 25);
        let banded = |app: &mut App| {
            let terminal = render(app);
            let buffer = terminal.backend().buffer();
            (0..buffer.area.height)
                .filter(|&y| {
                    let cell = &buffer[(buffer.area.width / 2, y)];
                    cell.style().bg == Some(band)
                })
                .count()
        };
        assert!(
            app.diff.as_ref().expect("diff view").referenced.is_none(),
            "a slide bands nothing: it already shows the stop's region"
        );
        assert_eq!(banded(&mut app), 0, "so no row is banded");

        let source = app.active_review_source();
        let note = app
            .review
            .session_for_mut(&source)
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: Some(3),
                    on_old_side: false,
                    line_text: None,
                },
                "agent",
                "two lines inside the region",
            )
            .id
            .clone();
        {
            let diff = app.diff.as_mut().expect("diff view");
            diff.layout = crate::config::FileLayout::Tree;
            diff.slide = None;
            diff.invalidate();
        }
        app.focus_comment(&note);
        assert!(
            banded(&mut app) >= 1,
            "outside the slide the file is all on screen, so the jumped-to span is banded"
        );
    }

    /// The card's rows shift the code, so a band keyed on row numbers would colour the whole slide.
    #[test]
    fn a_comment_added_inside_a_slide_leaves_it_unbanded() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        let band = crate::theme::blend(app.theme.bg, app.theme.accent, 25);

        let source = app.active_review_source();
        app.review.session_for_mut(&source).add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "why not 41?",
        );
        app.diff.as_mut().expect("diff view").invalidate();
        app.seat_stop(0);

        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let banded = (0..buffer.area.height)
            .filter(|&y| buffer[(buffer.area.width / 2, y)].style().bg == Some(band))
            .count();
        assert_eq!(banded, 0, "the slide stays plain with a comment in it");
    }

    #[test]
    fn a_slide_with_two_comments_renders_both_cards() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        let source = app.active_review_source();
        app.review.session_for_mut(&source).add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "why 42 and not 41?",
        );
        app.diff.as_mut().expect("diff view").invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_stops_notes_count_toward_its_sidebar_row() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();

        let stop = crate::mcp::StopParams {
            id: None,
            title: "The answer".to_owned(),
            anchor: Some("src/lib.rs#answer".to_owned()),
            body: "why 42".to_owned(),
            notes: Some(vec![
                crate::mcp::NoteParams {
                    id: None,
                    anchor: None,
                    body: "a first remark".to_owned(),
                },
                crate::mcp::NoteParams {
                    id: None,
                    anchor: Some("src/lib.rs:2".to_owned()),
                    body: "a second remark".to_owned(),
                },
            ]),
        };
        let crate::mcp::McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(crate::mcp::McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };

        app.open_walkthrough_diff(&published.id);
        let request = app
            .pending_walkthrough
            .take()
            .expect("resolution queued for the new walkthrough");
        let root = app.review.repo_root.clone();
        let contents = request
            .files
            .iter()
            .filter_map(|path| Some((path.clone(), std::fs::read_to_string(root.join(path)).ok()?)))
            .collect();
        app.handle(crate::event::AppEvent::WalkthroughAnchors {
            contents,
            pin_broken: false,
            token: request.token,
        });

        let screen = render(&mut app).backend().to_string();
        assert!(
            screen.contains(" · 3"),
            "the row counts the stop and both notes: {screen}"
        );
    }

    #[test]
    fn a_mermaid_fence_in_a_human_comment_draws_as_a_figure() {
        let (_fixture, mut app) = diff_app();
        let id = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                "does it go this way?\n\n```mermaid\nflowchart LR\n  a[read] --> b[merge]\n```\n",
            )
            .id
            .clone();
        app.diff.as_mut().expect("diff view").invalidate();
        app.focus_comment(&id);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// `GraphView::set_model` selects a node by default, and a card figure is a static picture.
    #[test]
    fn a_figures_selection_is_cleared_so_no_node_reverses_in_the_card() {
        let (_fixture, mut app) = diff_app();
        let id = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                "does it go this way?\n\n```mermaid\nflowchart LR\n  a[read] --> b[merge]\n```\n",
            )
            .id
            .clone();
        app.diff.as_mut().expect("diff view").invalidate();
        app.focus_comment(&id);
        let figure_rows = {
            let diff = app.diff.as_ref().expect("diff view");
            let cached = diff.figures.get(&id).expect("the figure is cached");
            cached
                .blocks
                .iter()
                .find_map(|block| match block {
                    crate::app::walkthrough::Block::Figure(figure) => Some(figure.rows()),
                    crate::app::walkthrough::Block::Prose(_) => None,
                })
                .expect("a figure block")
        };
        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let header_row = (area.y..area.y + area.height)
            .find(|&y| {
                let line: String = (area.x..area.x + area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                line.contains("figure 1")
            })
            .expect("the figure's header row is on screen");
        let last_row =
            (header_row + u16::try_from(figure_rows).unwrap_or(0)).min(area.y + area.height);
        let reversed = (header_row..last_row)
            .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                buffer[(x, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
            .count();
        assert_eq!(reversed, 0, "no node in the figure should render reversed");
    }

    fn walkthrough_app(fixture: &Fixture, stops: &[(&str, Option<&str>, &str)]) -> App {
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        crate::test_support::seat_walkthrough(&mut app, "How the answer moved", stops);
        app.open_walkthrough_diff("w1");
        // anchors resolve off-thread, so we land them before rendering
        let Some(request) = app.pending_walkthrough.take() else {
            return app;
        };
        let root = app.review.repo_root.clone();
        let read = diffler_core::review::Review::compute_walkthrough_files(
            &root,
            request.read_rev.as_deref(),
            request.read_first,
            &request.files,
        );
        app.handle(crate::event::AppEvent::WalkthroughAnchors {
            contents: read.contents,
            pin_broken: read.pin_broken,
            token: request.token,
        });
        app
    }

    /// A stop outside the diff makes `model_with_context` clone the model, which
    /// `ensure_rows` caches, so a render must not count a fresh merge.
    #[test]
    fn draw_pane_reads_the_cached_merged_model_instead_of_rebuilding_it() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(&fixture, &[("Notes", Some("notes.txt:1"), "why alpha")]);
        // the first render settles `walkthrough_built` and enrichment, which rebuild on their own
        render(&mut app);
        assert!(
            app.diff.as_ref().expect("diff view").merged_model.is_some(),
            "a context file forces a merged model"
        );
        let before = crate::app::merge_count();
        render(&mut app);
        render(&mut app);
        assert_eq!(
            crate::app::merge_count(),
            before,
            "draw_pane must read the cached merged model, not rebuild it"
        );
    }

    #[test]
    fn the_summary_slide_renders_one_card_and_no_diff_rows() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        crate::test_support::set_walkthrough_summary(&mut app, "w1", "the shape of the change");
        app.seat_summary();
        insta::assert_snapshot!(render(&mut app).backend());
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
        app.handle(key('m'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn the_comments_cursor_band_follows_focus_like_the_file_rows() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('l'));
        app.handle(key('j'));
        app.handle(key('c'));
        for c in "a note".chars() {
            app.handle(key(c));
        }
        app.handle(crate::test_support::code_key(
            crossterm::event::KeyCode::Enter,
        ));
        app.handle(key('C'));

        let band_of = |app: &mut App| {
            let terminal = render(app);
            let buffer = terminal.backend().buffer();
            let rect = app.diff.as_ref().expect("diff").comments_rect;
            buffer[(rect.x, rect.y)].style().bg
        };
        let focused = band_of(&mut app);
        // h leaves the comments pane for the diff, keeping the same selection
        app.handle(key('h'));
        let unfocused = band_of(&mut app);

        let theme = &app.theme;
        assert_eq!(focused, Some(super::sidebar_row_bg(theme, true, true)));
        assert_eq!(unfocused, Some(super::sidebar_row_bg(theme, true, false)));
        assert_ne!(focused, unfocused, "the band weakens with focus lost");
    }

    #[test]
    fn searching_the_comments_sidebar_lights_the_matches() {
        let (_fixture, mut app) = diff_app();
        for body in ["the widget leaks", "another widget here"] {
            app.review.session.add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                body,
            );
        }
        app.handle(key('C'));
        app.handle(key('/'));
        for c in "widget".chars() {
            app.handle(key(c));
        }

        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let rect = app.diff.as_ref().expect("diff").comments_rect;
        let painted = |bg| {
            (rect.y..rect.y + rect.height)
                .flat_map(|y| (rect.x..rect.x + rect.width).map(move |x| (x, y)))
                .filter(|at| buffer[*at].style().bg == Some(bg))
                .count()
        };
        assert_eq!(
            painted(app.theme.search_current),
            "widget".len(),
            "the active match is lit stronger"
        );
        assert_eq!(
            painted(app.theme.search),
            "widget".len(),
            "and the other match is lit too"
        );
    }

    #[test]
    fn kinds_layout_sidebar_groups_files_by_what_they_are() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Kinds;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn an_against_review_names_its_base_in_the_chip_and_the_pane_title() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_against_diff("main");
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn review_verdict_picker_renders_its_choices() {
        let (_fixture, mut app) = diff_app();
        let comment = crate::ci::NewPrComment {
            number: 7,
            head_oid: "abc".to_owned(),
            path: "src/lib.rs".to_owned(),
            line: Some(2),
            start_line: None,
            new_side: true,
            counterpart: None,
            body: "looks off".to_owned(),
        };
        let pending = crate::app::pr::PrPending {
            review_comments: vec![comment.clone(), comment],
            replies: vec![crate::app::pr::PrPost::Reply {
                number: 7,
                comment_id: "c1".to_owned(),
                reply_index: 0,
                parent_remote_id: "r1".to_owned(),
                body: "and this".to_owned(),
            }],
            agent_withheld: 2,
            file_level: 1,
            file_level_forge: Some(crate::ci::ProviderKind::Forgejo),
            ..Default::default()
        };
        app.modal = Some(crate::app::Modal::ReviewVerdict {
            number: 7,
            summary: pending.summary(),
        });
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn long_diff_lines_wrap_instead_of_clipping() {
        let fixture = standard_fixture();
        fixture.write(
            "src/lib.rs",
            &format!(
                "pub fn answer() -> u32 {{\n    42 // {}\n}}\n",
                "a very long trailing explanation that cannot possibly fit \
                 in the diff pane at the test terminal width"
                    .repeat(2)
            ),
        );
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        open_lib_diff(&mut app);
        let terminal = render(&mut app);
        let text = format!("{:?}", terminal.backend().buffer());
        assert!(
            text.contains("terminal width"),
            "the tail of the long line is on screen"
        );
        insta::assert_snapshot!(terminal.backend());
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
        // the textual engine word-diffs the `41`→`42` literal so the emphasis background composites
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.semantic_diff = false;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        open_lib_diff(&mut app);
        let terminal = render(&mut app);
        let styles = format!("{:?}", terminal.backend().buffer());
        let add_emph = format!("{:?}", app.theme.add_emph_bg);
        let del_emph = format!("{:?}", app.theme.del_emph_bg);
        assert!(styles.contains(&add_emph), "added emphasis bg rendered");
        assert!(styles.contains(&del_emph), "deleted emphasis bg rendered");
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
    fn only_the_pane_with_focus_carries_the_bright_cursor_band() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let band = app.theme.cursor_line;
        let bands = |app: &mut App| {
            let terminal = render(app);
            let buffer = terminal.backend().buffer().clone();
            let diff = app.diff.as_ref().expect("diff view");
            let count = |area: ratatui::layout::Rect| {
                (area.top()..area.bottom())
                    .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
                    .filter(|&(x, y)| buffer.cell((x, y)).is_some_and(|c| c.bg == band))
                    .count()
            };
            (count(diff.sidebar), count(diff.pane))
        };

        let (sidebar, pane) = bands(&mut app);
        assert_eq!(sidebar, 0, "the sidebar gives up its band to the diff pane");
        assert!(
            pane > 0,
            "the focused diff pane holds the bright cursor row"
        );

        app.diff.as_mut().expect("diff view").focus = Pane::List;
        let (sidebar, pane) = bands(&mut app);
        assert!(sidebar > 0, "focus moves the bright band to the sidebar");
        assert_eq!(pane, 0, "and the diff pane gives it up");
    }

    #[test]
    fn comment_range_highlights_its_anchored_lines() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(1),
                line_end: Some(2),
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
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

    #[test]
    fn zt_zb_zz_align_the_cursor_row_in_the_viewport() {
        let fixture = standard_fixture();
        let body: String = (0..100).fold(String::new(), |mut s, i| {
            use std::fmt::Write;
            let _ = writeln!(s, "line {i}");
            s
        });
        fixture.write("src/lib.rs", &body);
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        open_lib_diff(&mut app);
        render(&mut app);

        let mid = app.diff.as_ref().unwrap().rows.len() / 2;
        app.diff.as_mut().unwrap().cursor = mid;

        let pane_row_of = |app: &App, row: usize| {
            app.diff
                .as_ref()
                .unwrap()
                .line_rows
                .iter()
                .position(|r| *r == Some(row))
        };

        // the margin the view keeps applies to the align commands too, as it
        // does in vim: `zt` stops `scrolloff` rows short of the top
        app.handle(key('z'));
        app.handle(key('t'));
        render(&mut app);
        assert_eq!(
            pane_row_of(&app, mid),
            Some(crate::ui::SCROLLOFF),
            "zt puts the cursor at the top, less the margin"
        );

        app.handle(key('z'));
        app.handle(key('b'));
        render(&mut app);
        let viewport = app.diff.as_ref().unwrap().viewport as usize;
        assert_eq!(
            pane_row_of(&app, mid),
            Some(viewport - 1 - crate::ui::SCROLLOFF),
            "zb puts the cursor at the bottom, less the margin"
        );

        app.handle(key('z'));
        app.handle(key('z'));
        render(&mut app);
        let at = pane_row_of(&app, mid).expect("cursor visible after zz");
        let center = viewport / 2;
        assert!(
            at.abs_diff(center) <= 1,
            "zz centers the cursor (row {at}, center {center})"
        );
    }

    #[test]
    fn fenced_code_block_in_a_comment_is_syntax_highlighted() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "try this:\n```rust\nfn answer() -> u32 { 42 }\n```",
        );
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn md_span_applies_the_syntax_color() {
        let theme = crate::theme::Theme::github_dark();
        let run = crate::app::markdown::MdSpan {
            text: "fn".to_owned(),
            code: true,
            fg: Some((1, 2, 3)),
            ..Default::default()
        };
        let span = super::md_span(&run, theme.base(), &theme);
        assert_eq!(
            span.style.fg,
            Some(ratatui::style::Color::Rgb(1, 2, 3)),
            "fg overrides the code accent"
        );
    }

    #[test]
    fn markdown_in_a_comment_body_renders_styled() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "call `foo()` then make it **bold**",
        );
        app.diff.as_mut().unwrap().invalidate();
        let terminal = render(&mut app);
        let buffer = format!("{:?}", terminal.backend().buffer());
        assert!(
            buffer.contains("BOLD"),
            "the **bold** run renders with the bold modifier"
        );
        assert!(
            buffer.contains(&format!("{:?}", app.theme.accent)),
            "the `code` run renders in the accent color"
        );
        insta::assert_snapshot!(terminal.backend());
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
        assert!(app.composer_open(), "double-click opened the composer");
    }

    #[test]
    fn a_click_outside_the_comment_box_keeps_the_draft_for_reopening() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let (x, y0, ..) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(x, y0));
        for c in "half a thought".chars() {
            app.handle(key(c));
        }
        let kind = app
            .diff
            .as_ref()
            .unwrap()
            .composer
            .as_ref()
            .unwrap()
            .kind
            .clone();
        render(&mut app);
        let sidebar = app.diff.as_ref().unwrap().sidebar;
        app.handle(mouse_click(sidebar.x + 1, sidebar.y));
        assert!(!app.composer_open(), "the click closed the box");
        app.open_composer(kind, String::new());
        let reopened = app.diff.as_ref().unwrap().composer.as_ref().unwrap();
        assert_eq!(reopened.buffer, "half a thought", "the draft came back");
    }

    #[test]
    fn a_right_click_on_a_folder_leaves_it_open() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let diff = app.diff.as_ref().unwrap();
        let rows = diff.sidebar_tree(&app.review);
        let folder = rows
            .iter()
            .position(|row| row.node.is_group())
            .expect("a folder row");
        let before = rows.len();
        let y = diff.sidebar.y + u16::try_from(folder - diff.sidebar_scroll).unwrap();
        app.handle(mouse_right_click(diff.sidebar.x + 2, y));
        let after = app.diff.as_ref().unwrap().sidebar_tree(&app.review).len();
        assert_eq!(
            after, before,
            "the menu acts on the folder without folding it"
        );
    }

    #[test]
    fn a_kept_edit_draft_comes_back_over_the_original_text() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let kind = crate::app::composer::ComposerKind::Edit {
            comment_id: "c1".to_owned(),
        };
        let edited = crate::app::composer::Composer::new(kind.clone(), "edited".to_owned());
        let other = crate::app::composer::Composer::new(
            crate::app::composer::ComposerKind::Reply {
                comment_id: "c2".to_owned(),
            },
            "a reply".to_owned(),
        );
        app.diff.as_mut().unwrap().parked_drafts = vec![edited, other];
        app.open_composer(kind, "original".to_owned());
        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.composer.as_ref().unwrap().buffer, "edited");
        assert_eq!(diff.parked_drafts.len(), 1, "the other draft is still kept");
    }

    #[test]
    fn t_regroups_only_the_list_that_has_the_keyboard() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let before = app.diff.as_ref().unwrap().layout;
        app.diff.as_mut().unwrap().focus = Pane::Diff;
        app.handle(key('t'));
        assert_eq!(
            app.diff.as_ref().unwrap().layout,
            before,
            "the diff pane leaves it"
        );
        app.diff.as_mut().unwrap().focus = Pane::List;
        app.handle(key('t'));
        assert_ne!(
            app.diff.as_ref().unwrap().layout,
            before,
            "the file list regroups"
        );
    }

    #[test]
    fn a_click_on_a_folder_folds_it() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let diff = app.diff.as_ref().unwrap();
        let rows = diff.sidebar_tree(&app.review);
        let folder = rows
            .iter()
            .position(|row| matches!(row.node, crate::tree::TreeNode::Dir { .. }))
            .expect("a folder row");
        let before = rows.len();
        let y = diff.sidebar.y + u16::try_from(folder - diff.sidebar_scroll).unwrap();
        let x = diff.sidebar.x + 2;
        app.handle(mouse_click(x, y));
        let after = app.diff.as_ref().unwrap().sidebar_tree(&app.review).len();
        assert!(
            after < before,
            "the folder's files are hidden: {before} -> {after}"
        );
    }

    #[test]
    fn right_click_over_a_selection_offers_to_comment_on_the_range() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let (x, y0, y1, ..) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_drag(x, y1));
        app.handle(mouse_right_click(x, y0));
        assert!(
            app.diff.as_ref().unwrap().visual_anchor.is_some(),
            "the selection stays for the menu to act on"
        );
        let Some(crate::app::Modal::Menu { commands, .. }) = &app.modal else {
            panic!("a menu opened");
        };
        assert_eq!(
            commands.first().map(|command| command.action),
            Some(crate::keymap::Action::Comment)
        );
    }

    #[test]
    fn right_click_on_a_line_lists_its_verbs_and_a_click_runs_one() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let (x, y0, ..) = first_two_pane_lines(&app);
        app.handle(mouse_right_click(x, y0));
        let Some(crate::app::Modal::Menu { commands, .. }) = &app.modal else {
            panic!("a menu opened");
        };
        assert!(
            commands
                .iter()
                .any(|command| command.label == "comment on this line"),
            "{commands:?}"
        );
        insta::assert_snapshot!(render(&mut app).backend());
        app.handle(key('\n'));
        assert!(app.composer_open(), "the first entry, comment, ran");
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
        let rc = super::TreeRowCtx {
            theme: &app.theme,
            depth: 0,
            width: 80,
            on_cursor: false,
            focused: true,
            search: &[(0..3, true)],
        };
        let spans = super::sidebar_file_line(&rc, &file, "lib.rs", false, 0);
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
    fn a_directory_header_sums_the_diffstat_of_every_file_below_it() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("src/a.rs", "one\n");
        fixture.write("src/nested/b.rs", "one\n");
        fixture.write("top.txt", "one\n");
        fixture.commit_all("base");
        // 2 added 1 deleted under src/, split across two depths
        fixture.write("src/a.rs", "two\n");
        fixture.write("src/nested/b.rs", "one\nthree\n");
        fixture.write("top.txt", "one\nfour\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);

        let diff = app.diff.as_ref().expect("diff view");
        let session = app.review.session_for(&diff.source);
        let stat = super::GroupStat::collect(diff, diff.model(&app.review), session);
        assert_eq!(stat.dirs.get("src"), Some(&(2, 1)));
        assert_eq!(stat.dirs.get("src/nested"), Some(&(1, 0)));
        assert_eq!(stat.dirs.get("top.txt"), None, "a file is not a group");

        let text = render(&mut app).backend().to_string();
        assert!(text.contains("+2 -1"), "src/ shows its diffstat: {text}");
    }

    #[test]
    fn a_kind_header_sums_the_diffstat_of_its_bucket() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("src/a.rs", "one\n");
        fixture.write("src/b.rs", "one\n");
        fixture.write("README.md", "docs\n");
        fixture.commit_all("base");
        fixture.write("src/a.rs", "one\ntwo\n");
        fixture.write("src/b.rs", "one\nthree\n");
        fixture.write("README.md", "more docs\n");
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Kinds;
        let mut app = App::new(fixture.review(), loaded);
        app.open_working_tree_diff(None);
        let text = render(&mut app).backend().to_string();
        assert!(
            text.contains("Source (2)") && text.contains("+2 -0"),
            "Source keeps its count and adds up its files: {text}"
        );
        assert!(text.contains("Docs (1)"), "the count stays: {text}");
    }

    #[test]
    fn the_review_buckets_split_the_diffstat_by_what_is_left() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("a.rs", "one\n");
        fixture.write("b.rs", "one\n");
        fixture.commit_all("base");
        fixture.write("a.rs", "one\ntwo\n");
        fixture.write("b.rs", "one\nthree\nfour\n");
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Review;
        let mut app = App::new(fixture.review(), loaded);
        app.open_working_tree_diff(None);
        let hash = app
            .review
            .model()
            .files
            .iter()
            .find(|file| file.path == "a.rs")
            .expect("a.rs in the diff")
            .content_hash();
        app.review
            .session_for_mut(&super::ReviewSource::WorkingTree)
            .mark_viewed("a.rs", &hash);

        let diff = app.diff.as_ref().expect("diff view");
        let session = app.review.session_for(&diff.source);
        let stat = super::GroupStat::collect(diff, diff.model(&app.review), session);
        assert_eq!(stat.sections.get(&super::Bucket::Viewed), Some(&(1, 0)));
        assert_eq!(stat.sections.get(&super::Bucket::ToReview), Some(&(2, 0)));
    }

    #[test]
    fn a_group_header_carries_the_same_diffstat_colors_as_a_file_row() {
        let theme = Theme::github_dark();
        let rc = super::TreeRowCtx {
            theme: &theme,
            depth: 0,
            width: 40,
            on_cursor: false,
            focused: true,
            search: &[],
        };
        let spans = super::sidebar_dir_line(&rc, "src", false, (3, 1));
        let painted: Vec<(&str, Option<super::Color>)> = spans
            .spans
            .iter()
            .filter(|span| span.content.contains('+') || span.content.contains('-'))
            .map(|span| (span.content.as_ref(), span.style.fg))
            .collect();
        assert_eq!(
            painted,
            vec![(" +3", Some(theme.added)), (" -1", Some(theme.error_fg))],
            "the header reuses the file row's diffstat, no bar: {spans:?}"
        );
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
        // f00 also shows in the pane header, so we check f01 to prove the sidebar scrolled
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
        assert!(
            content.contains(".."),
            "range header shows a span: {content}"
        );
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_long_thread_folds_and_splits_into_lanes() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let human = app.author.clone();
        let id = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("pub fn answer() -> u32 {".to_owned()),
                },
                &human,
                "why full jitter here?",
            )
            .id
            .clone();
        let session = &mut app.review.session;
        session.reply(&id, "agent", "it spreads the restarts after a deploy");
        session.reply(&id, &human, "fair, but cap it");
        session.reply(&id, &human, "and the docs?");
        session.reply(&id, "agent", "capped at two seconds\n\nadded a test for attempt zero\n\nupdated the docs\n\nand the changelog\n\nand the bench");
        app.diff.as_mut().unwrap().invalidate();
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
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("pub fn answer() -> u32 {".to_owned()),
                },
                "reviewer",
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
    fn a_markdown_table_in_a_comment_renders_as_columns() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.review.session.add_comment(
            diffler_core::session::Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                line_text: Some("    42".to_owned()),
            },
            "reviewer",
            "| Between | Ordering | What a failure costs |\n\
             |---|---|---|\n\
             | Two customers | Parallel | Nothing, one lane throwing leaves the others |\n\
             | Postgres, then the sinks | Strictly ordered | No sink hears anything this run |",
        );
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn long_comment_text_wraps_to_the_pane_width() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        let id = app
            .review
            .session
            .add_comment(
                diffler_core::session::Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("    42".to_owned()),
                },
                "reviewer",
                "a negative or far too large answer breaks the callers downstream, \
                 so this needs a clamp before it ships to anyone",
            )
            .id
            .clone();
        app.review.session.reply(
            &id,
            "agent",
            "agreed, clamping to the documented range and adding a regression \
             test so the next refactor cannot silently drop it",
        );
        app.diff.as_mut().unwrap().invalidate();
        let terminal = render(&mut app);
        insta::assert_snapshot!(terminal.backend());
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
        let lit = terminal.backend().buffer().clone();
        app.diff.as_mut().unwrap().visual_anchor = None;
        let plain = render(&mut app).backend().buffer().clone();
        let repainted = lit
            .content
            .iter()
            .zip(plain.content.iter())
            .filter(|(a, b)| a.bg != b.bg)
            .count();
        assert!(repainted > 0, "selection must paint extra rows");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn viewed_file_shows_a_check_and_comment_count_in_the_sidebar() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
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
        app.handle(key('m'));
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("viewed 1/3 files"), "{content}");
    }

    #[test]
    fn viewed_walk_advances_the_selection() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('m'));
        app.handle(key('m'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn viewed_files_lead_with_a_check_and_sort_above_the_rest() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "one\n");
        fixture.write("b.txt", "one\n");
        fixture.write("c.txt", "one\n");
        fixture.commit_all("base");
        fixture.write("a.txt", "one\ntwo\n");
        fixture.write("b.txt", "one\ntwo\n");
        fixture.write("c.txt", "one\ntwo\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let model = app.review.model().clone();
        let hash_of = |path: &str| {
            model
                .files
                .iter()
                .find(|f| f.path == path)
                .expect("file in the diff")
                .content_hash()
        };
        let session = app
            .review
            .session_for_mut(&super::ReviewSource::WorkingTree);
        session.mark_viewed("a.txt", &hash_of("a.txt"));
        session.mark_viewed("b.txt", &hash_of("b.txt"));
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn markdown_diff_highlights_headings_and_inline_code() {
        let fixture = Fixture::new();
        fixture.write(
            "notes.md",
            "# Recording notes\n\nUse `record()` and set **duration** first.\n\n1. call `search(q)`\n2. share it\n",
        );
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("notes.md");
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn expand_whole_file_reveals_the_unchanged_lines() {
        use std::fmt::Write as _;
        let fixture = Fixture::new();
        let mut base = String::new();
        for i in 1..=10 {
            let _ = writeln!(base, "line {i}");
        }
        fixture.write("notes.txt", &base);
        fixture.commit_all("base");
        fixture.write("notes.txt", &base.replace("line 5\n", "LINE FIVE\n"));
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("notes.txt");
        app.handle(key('='));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_folded_hunk_reads_as_its_header_naming_what_it_hides() {
        let base: String = (1..=30)
            .flat_map(|i| ["fn f", &i.to_string(), "() {}\n"].map(str::to_owned))
            .collect();
        let fixture = Fixture::new();
        fixture.write("f.rs", &base);
        fixture.commit_all("base");
        fixture.write(
            "f.rs",
            &base
                .replace("fn f5()", "fn five()")
                .replace("fn f20()", "fn twenty()"),
        );
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("f.rs");
        let _ = render(&mut app);
        let diff = app.diff.as_mut().expect("diff");
        diff.focus = Pane::Diff;
        diff.cursor = 0;
        app.handle(key('z'));
        app.handle(key('a'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// Several themes put the cursor band within a shade of the hunk band, so both take the accent.
    #[test]
    fn a_fold_row_shares_the_hunk_band_and_lights_its_text_under_the_cursor() {
        use crate::ui::diff_render::{fold_row, hunk_header};
        for name in crate::theme::NAMES {
            let theme = Theme::from_name(name).0;
            let hunk = diffler_core::model::Hunk {
                id: diffler_core::model::HunkId("h".into()),
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                context: String::new(),
                lines: Vec::new(),
            };
            let bg = |line: &ratatui::text::Line<'_>| line.spans[0].style.bg;
            let fg = |line: &ratatui::text::Line<'_>| line.spans[0].style.fg;
            let fold = fold_row(&theme, "⋯ 4 lines · fn b", 40, false, true);
            assert_eq!(bg(&fold), bg(&hunk_header(&theme, &hunk, 40, false, true)));
            let lit = fold_row(&theme, "⋯ 4 lines · fn b", 40, true, true);
            assert_eq!(fg(&lit), Some(theme.accent), "{name}");
            assert_eq!(lit.width(), 40, "{name}");
        }
    }

    #[test]
    fn a_reformat_only_pair_reads_as_dimmed_context() {
        let fixture = Fixture::new();
        fixture.write(
            "src/main.rs",
            "fn main() {\n    let total=add(1,2);\n    let label = name(total);\n}\n",
        );
        fixture.commit_all("base");
        fixture.write(
            "src/main.rs",
            "fn main() {\n    let total = add(1, 2);\n    let label = title(total);\n}\n",
        );
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("src/main.rs");
        app.apply_diff_algorithm("structural");
        app.settle_rediff();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn expansion_reflows_rows_after_a_refresh_re_enriches() {
        use std::fmt::Write as _;
        let fixture = Fixture::new();
        let mut base = String::new();
        for i in 1..=40 {
            let _ = writeln!(base, "line {i}");
        }
        fixture.write("a.txt", &base);
        fixture.commit_all("base");
        fixture.write("a.txt", &base.replace("line 20\n", "LINE TWENTY\n"));
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("a.txt");

        app.handle(key('=')); // whole-file expand
        let _ = render(&mut app); // draw the expanded rows, clearing rows_dirty

        // a real content change (as a watcher refresh would see) rebuilds the
        // model at default context; the per-file override lives on the view
        fixture.write("a.txt", &base.replace("line 30\n", "LINE THIRTY\n"));
        app.handle(crate::event::AppEvent::RepoChanged);
        app.settle_refresh();
        app.queue_enrich_selected(); // enrichment reinstalls the override in on_enriched
        let _ = render(&mut app);

        let rows = app.diff.as_ref().expect("diff").rows().len();
        assert!(
            rows > 20,
            "rows re-flow to the whole file after re-enrich, got {rows}"
        );
    }

    #[test]
    fn diff_pane_renders_a_sliding_window_and_jumps_to_extremes() {
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
    fn stray_clicks_never_lose_a_draft() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "half written".chars() {
            app.handle(key(c));
        }
        render(&mut app);
        // a double-click that opens a composer on another line, then a click
        // on the file sidebar
        let (x, y0, ..) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(1, 3));
        let diff = app.diff.as_ref().unwrap();
        let kept = diff
            .composer
            .iter()
            .chain(&diff.parked_drafts)
            .any(|draft| draft.buffer == "half written");
        assert!(kept, "the draft is open or kept aside for reopening");
    }

    #[test]
    fn a_file_level_composer_renders_at_the_top_of_the_pane() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        app.diff.as_mut().unwrap().focus = Pane::List;
        app.handle(key('c'));
        for c in "applies to the whole file".chars() {
            app.handle(key(c));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_reply_composer_renders_under_the_thread_it_answers() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "why?".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        settle_submit(&mut app);
        let comment_row = app
            .diff
            .as_ref()
            .unwrap()
            .rows()
            .iter()
            .position(|row| matches!(row, crate::app::DiffRow::Comment { line: 0, .. }))
            .expect("the comment header");
        app.diff.as_mut().unwrap().cursor = comment_row;
        app.handle(key('r'));
        for c in "because".chars() {
            app.handle(key(c));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn the_composer_renders_under_the_line_it_comments_on() {
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
