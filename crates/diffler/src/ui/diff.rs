//! Diff/review screen: a two-pane layout with a left file sidebar listing every
//! file in the diff (status, viewed mark, comment count) and a right pane that
//! renders the visible slice of the selected file's hunks, lines, and inline
//! comment blocks, keeping the cursor in view.

use std::collections::HashMap;

use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffLine, DiffModel, FileDiff};
use diffler_core::session::{Comment, CommentStatus, Session};
use diffler_core::source::ReviewSource;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::composer::{Composer, ComposerKind, ComposerLine};
use crate::app::markdown::MdSpan;
use crate::app::rowsel::RowSelect;
use crate::app::walkthrough::{Block as WalkthroughBlock, stop_title, summary_figure_key};
use crate::app::{
    App, CommentFacts, CommentGrouping, CommentLine, CommentPaneRow, DiffRow, DiffView,
    FileHighlights, FileScope, Pane, RowCopy, SplitRow, SplitSide, comment_display,
    group_comment_rows, summary_display,
};
use crate::config::FileLayout;
use crate::keymap::Action;
use crate::search::Search;
use crate::theme::Theme;
use crate::tree::{Bucket, TreeNode};
use crate::ui::Hint;
use crate::ui::diff_render::{
    LineFlags, PairSelection, align_scroll, card_frame, cursor_band, diff_line_height,
    file_gutter_width, hunk_header, line_syntax, render_diff_line, render_split_pair,
    split_pair_height,
};
use crate::ui::{diffstat_spans, proportion_bar, status_bar, status_color};

/// Hint entries, rendered against the live keymap so remaps show.
const HINTS: &[Hint] = &[
    Hint::Leaf(&[Action::Comment], "add comment"),
    Hint::Leaf(&[Action::Reply], "reply"),
    Hint::Leaf(&[Action::MarkViewed], "mark viewed"),
    Hint::Leaf(&[Action::CommentsOverview], "see comment list"),
    Hint::Leaf(&[Action::Help], "help"),
];

/// Sidebar width: a quarter of the screen, clamped to a readable band.
fn sidebar_width(total: u16) -> u16 {
    (total / 4).clamp(28, 44).min(total)
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (body, bar) = super::screen_chrome(frame, app, HINTS);

    // a figure that needed help fitting names the key that opens it
    // full-screen, read against the live keymap so a remap still shows
    let open_figure_hint = app.active_keymap().chord_for(Action::OpenFigureGraph);
    // enrichment (emphasis/highlight/scope) runs on the blocking pool; this
    // only queues work, and the pane renders plain until the result lands
    app.queue_enrich_selected();
    // comment wrap follows the diff pane's inner width: the body minus the
    // sidebar column and the pane block's two border columns. The bodies are
    // parsed to it, so the width has to be in before anything reads them
    let pane_width = body.width.saturating_sub(sidebar_width(body.width) + 2);
    if let Some(diff) = app.diff.as_mut() {
        diff.set_wrap_width(pane_width);
    }
    // an agent republishing under a reader has to show; this only queues work
    app.ensure_walkthrough_view();

    // disjoint field borrows: the diff view mutates (scroll, highlight
    // cache) while theme and review stay read-only
    let theme = &app.theme;
    let review = &app.review;
    let search = app.search.as_ref();
    let highlighter = app.highlighter.as_ref();
    let human_author = app.author.as_str();
    if let Some(diff) = app.diff.as_mut() {
        diff.ensure_rows(review);
        // the source is cloned out so the session's borrow is off the view,
        // which the rasteriser needs mutably
        let source = diff.source.clone();
        let session = review.session_for(&source);
        // rasterising a figure needs the graph mutably, and the pane's loop
        // holds the model borrowed off the same view: do them all up front
        let rasters = rasterize_figures(diff, theme, pane_width, open_figure_hint.as_deref());
        // a figure's box-drawing text only exists once it is drawn, so a
        // selection covering it copies exactly what this pass just rasterised
        patch_figure_copy_text(diff, &rasters);
        // a commit view renders from its pinned model; only fall back to the
        // (lazily computed) working-tree model for the working-tree view.
        // Context files are not folded in here: they live on `diff`, and this
        // reference has to survive passing `diff` itself into `draw_body`
        // below, so each renderer that needs them reads `diff.context_files`
        // directly instead (a fresh, disjoint borrow of its own parameter).
        let review_model = (diff.commit_model.is_none()).then(|| review.model());
        let ctx = RenderCtx {
            theme,
            session,
            review_model,
            search,
            highlighter,
            human_author,
            rasters: &rasters,
        };
        draw_body(frame, body, &ctx, diff);
    }

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
    /// The reviewer's own author name (`App::author`), so a comment's colour
    /// can tell "you" apart from everyone else without a second source.
    human_author: &'a str,
    /// Every card's figures, drawn once per frame and keyed by `(id, block)`
    /// (a comment's id, or the walkthrough's own summary key); a card row
    /// then only reads a line out of one.
    rasters: &'a FigureRaster,
}

/// The rendered rows of every figure a card draws, by the card's figure-cache
/// key and the block they belong to.
type FigureRaster = HashMap<(String, usize), Vec<Line<'static>>>;

/// Draw every figure the open view's cards hold into lines: every comment's
/// and the walkthrough's own summary alike, since both cache their bodies the
/// same way. Figures are static in the pane, so one pass per frame serves
/// every row that shows part of one.
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

/// Resolve every figure row's copy text from the lines this pass just drew: a
/// plain-text builder cannot reproduce the graph renderer's layout, so a
/// figure row starts as a lookup key (see [`RowCopy`]) and is patched here,
/// the one place the box-drawing already exists.
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

/// Whether a row sits under the cursor, and whether its pane holds focus
/// (focus decides how bright the cursor band renders).
#[derive(Clone, Copy)]
struct RowState {
    selected: bool,
    focused: bool,
}

/// A sidebar tree row's shared rendering inputs: theme, its indent depth, the
/// pane width, cursor/focus state, and any search ranges to highlight.
#[derive(Clone, Copy)]
struct TreeRowCtx<'a> {
    theme: &'a Theme,
    depth: usize,
    width: u16,
    on_cursor: bool,
    focused: bool,
    search: &'a [(std::ops::Range<usize>, bool)],
}

/// The side-by-side view's currently open file: its diff, cached highlights,
/// and gutter width, constant for every row while that file is open.
#[derive(Clone, Copy)]
struct SplitFileCtx<'a> {
    file: &'a FileDiff,
    highlights: Option<&'a FileHighlights>,
    gutter: usize,
}

/// Columns of empty background between the two panes. The gap is what reads
/// as their divider.
const PANE_GAP: u16 = 1;

fn draw_body(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    // the sidebar takes one column beyond its content so the gap and the
    // pane's own left column both stay clear of it
    let width = (sidebar_width(area.width) + 1).min(area.width);
    let comments = comments_width(area.width, diff.comments_open);
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
    if comments > 0 {
        draw_comments(frame, comments_area, ctx, diff);
    }
}

/// The comments sidebar mirrors the file list's band, and yields the whole
/// width back when closed.
fn comments_width(total: u16, open: bool) -> u16 {
    if !open {
        return 0;
    }
    sidebar_width(total).min(total / 3)
}

/// What a card needs to light its search matches: the live query, and whether
/// this card holds the active match so its hits take the stronger colour.
#[derive(Clone, Copy)]
struct CardSearch<'a> {
    query: &'a str,
    current: bool,
}

/// A comment card's shared rendering inputs, the sidebar's counterpart to
/// [`TreeRowCtx`].
#[derive(Clone, Copy)]
struct CardCtx<'a> {
    theme: &'a Theme,
    budget: usize,
    bg: Color,
    width: u16,
    /// Indent under the group header the way a file indents under its
    /// directory; 0 for a flat list, which has no header to nest under.
    depth: usize,
    on_cursor: bool,
    orphan: bool,
    /// The author's own colour: stepped from where the author first appears
    /// in the pane, fixed instead for the human and the agent, so the
    /// reader's eye finds those two without reading.
    author_color: Color,
    search: Option<CardSearch<'a>>,
}

/// The pane's rows under its current grouping, built from the same ordering
/// (`ordered_comments`) `App::comment_rows` sorts before it groups, so the
/// two never disagree on which row a click or a keystroke lands on.
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

/// A group header row's shared rendering inputs, trimmed down to what
/// `group_header_line` needs beyond label/count/fold/tail.
#[derive(Clone, Copy)]
struct HeaderCtx<'a> {
    theme: &'a Theme,
    bg: Color,
    width: u16,
    on_cursor: bool,
    rail: ViewedRail,
}

/// A group header row: fold arrow, bold label, its count, and an optional
/// right-aligned tail. Shared by the file sidebar's own sections (a diffstat
/// tail and a viewed rail) and the comments pane's (neither, since a comment
/// carries no diff and no viewed state of its own).
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
        rail,
    } = hc;
    let arrow = if folded { "▸ " } else { "▾ " };
    let label_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let mut spans = vec![
        tree_lead(theme, 0, bg, on_cursor, rail),
        Span::styled(arrow.to_owned(), dim),
        Span::styled(label.to_owned(), label_style),
        Span::styled(format!(" ({count})"), dim),
    ];
    push_right(&mut spans, tail, width, bg);
    pad_line(spans, bg, width)
}

/// A comments-pane group header: `group_header_line` with no tail and no rail.
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
            rail: ViewedRail::None,
        },
        label,
        count,
        folded,
        Vec::new(),
    )
}

/// Right pane: the review's comments under the pane's own grouping, each a
/// header line (file, line, status) and its body wrapped to the column. The
/// selection drives the diff cursor, so the highlighted card is always the
/// one the pane's verbs act on.
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

    let (mut lines, owners, cursor_line) =
        comment_pane_lines(ctx, diff, &rows, &ordered, inner, focused);
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

/// Every row of the comments pane flattened to rendered lines: one entry per
/// line back to the row it belongs to, so a click on any wrapped body line
/// selects the header or comment it came from, plus where the cursor's own
/// line landed so the pane can scroll to it.
fn comment_pane_lines(
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    rows: &[CommentPaneRow],
    ordered: &[(&diffler_core::session::Comment, bool)],
    inner: Rect,
    focused: bool,
) -> (Vec<Line<'static>>, Vec<Option<usize>>, usize) {
    let theme = ctx.theme;
    let search = ctx.search;
    let surface = sidebar_bg(theme);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut owners: Vec<Option<usize>> = Vec::new();
    let budget = (inner.width as usize).saturating_sub(2).max(1);
    // a flat list has no header to nest items under; every other grouping
    // indents its items one level, the way a file indents under its directory
    let item_depth = usize::from(diff.comment_grouping != CommentGrouping::Flat);
    let orders = author_orders(
        ordered.iter().map(|(comment, _)| comment.author.as_str()),
        ctx.human_author,
    );
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
                // every comment is one line, the cursor's included: the diff
                // pane already shows the one it seats, and a row that grew
                // under the cursor moved every row below it on each step
                lines.push(comment_summary_line(&card, comment));
                owners.push(Some(row_index));
                // a spacer trails a group's last item, so a busy pane reads
                // dense and not as a wall of gaps; a header carries no spacer
                // of its own, the same density the file sidebar's sections keep
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

/// The review's comments in sidebar order: by file as the diff lists them,
/// then by line, matching `App::comment_order`. A file the diff no longer
/// carries ranks last, which is the same thing as being orphaned, so each
/// comment comes back paired with that answer.
fn ordered_comments<'a>(
    ctx: &'a RenderCtx<'_>,
    diff: &DiffView,
) -> Vec<(&'a diffler_core::session::Comment, bool)> {
    let rank = |path: &str| {
        diff.commit_model
            .as_ref()
            .or(ctx.review_model)
            .and_then(|model| model.files.iter().position(|file| file.path == path))
            .unwrap_or(usize::MAX)
    };
    let mut ordered: Vec<(&diffler_core::session::Comment, usize)> = ctx
        .session
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

/// One comment as a header line plus its wrapped body.
/// One line's search matches, in the shape `highlight_spans` paints everywhere.
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

/// A hue turned into a saturated colour by `author_color`'s golden-angle
/// step, lifted through [`readable_on`](diffler_core::language::readable_on)
/// the same way `crate::ui::language_color` lifts Linguist's palette.
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
        // scaled into 0.0..=255.0 by the clamp just above the cast
        #[allow(clippy::cast_sign_loss)]
        let byte = ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        byte
    };
    (channel(r1), channel(g1), channel(b1))
}

/// The golden angle (~137.5°): stepping a hue by it spaces each new one as far
/// as possible from every hue before it, the same trick sunflower seeds use to
/// pack without two ever landing too close.
const HUE_STEP: f32 = 137.507_76;

/// Each author's position in the pane's own order, first appearance first,
/// skipping the human and the agent: they take a fixed colour, so they never
/// consume a step and never collide with one either.
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

/// An author's colour: fixed for the two names that never move (the human
/// reviewing, the agent replying) since the reader looks for those first,
/// stepped by the golden angle from `order` for anyone else so a handful of
/// reviewers read as visibly distinct hues rather than colliding on a hash.
/// Lifted for contrast against `bg`, the row's own background, so it stays
/// legible on any theme and under the cursor's own band.
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

/// A comment not under the cursor draws as one line: the status glyph and
/// author lead it exactly as the open card's header does, then as much of
/// its preview as the row holds.
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

/// The header spans every card leads with: status glyph, then the author in
/// its own colour, at whatever width `rest_budget` still has room for once
/// they are placed.
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
    // an orphan outranks its status: the file it points at is gone, which is
    // the only thing worth saying about it
    let (status, colour) = match comment.status {
        _ if orphan => ("⚠", theme.error_fg),
        CommentStatus::Open => ("○", theme.warn_fg),
        CommentStatus::Replied => ("◐", theme.accent),
        CommentStatus::Resolved => ("✓", theme.added),
    };
    let spans = vec![
        tree_lead(theme, depth, bg, on_cursor, ViewedRail::None),
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

/// Columns a name may take on a comment row. A long handle would otherwise
/// fill the row and leave the preview beside it nothing to say.
const AUTHOR_MAX: usize = 14;

/// A comment not under the cursor: the status and author its own card leads
/// with, then as much of its preview as the row still holds, elided.
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

/// Left pane: a heading row then one row per file in the diff, the selected
/// one highlighted.
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
    // build only the visible slice: the tree can be far taller than the pane
    // and styling every row per frame is O(files)
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
            // ranges are offsets into the row's name, so the `/` match
            // highlights the exact substring like the log and diff panes do
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
                    ViewedRail::of_group(stat.dirs_viewed.get(path).copied()),
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
                    ViewedRail::of_group(stat.sections_viewed.get(bucket).copied()),
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

/// What the sidebar is listing: files, or the walkthrough by name, since a
/// stop list is only readable when the reader knows whose order it is. A
/// broken pin says so here, since it is a fact about the whole walkthrough,
/// not any one stop.
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

/// A walkthrough stop row: its title, the file it is anchored to dimmed after
/// it, and how many comments its region holds once that is more than the stop
/// itself.
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
    // stops keep their reading order regardless of what is seen, so a rail
    // here would not read as a run the way a sorted file list does; the `✓`
    // already says a stop is seen
    let mut spans = vec![tree_lead(theme, 0, bg, on_cursor, ViewedRail::None)];
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

/// The walkthrough layout's leading row, shown only where the walkthrough has
/// a summary: no count, since it is one card, not a bucket of stops.
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
    let mut spans = vec![tree_lead(theme, 0, bg, on_cursor, ViewedRail::None)];
    spans.extend(super::highlight_spans(
        "Summary",
        title_style,
        search,
        theme,
    ));
    pad_line(spans, bg, width)
}

/// The last segment of a path, which is how a file is named in a list beside
/// something else.
pub(crate) fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// Right pane: the selected file's header then the visible slice of its rows,
/// in unified or side-by-side mode.
#[allow(clippy::too_many_lines)]
fn draw_pane(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx<'_>, diff: &mut DiffView) {
    let (theme, session, review_model, search) =
        (ctx.theme, ctx.session, ctx.review_model, ctx.search);
    let focused = diff.focus == Pane::Diff;
    let title = pane_title(&diff.source);
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
    // long lines wrap, so rows vary in height: place every row first, then
    // scroll in visual lines keeping the whole cursor row on screen
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
        // only the focused pane highlights; otherwise the sidebar's
        // matches (keyed by row index) would bleed onto diff rows
        let ranges = search
            .filter(|_| focused)
            .map(|s| s.ranges_for(index))
            .unwrap_or_default();
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
            &ranges,
        );
        // a stop points at a segment, so the whole span is banded; the cursor
        // row keeps its own band, which is what says where inside the span the
        // reader is standing
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

/// Terminal rows a split row occupies at `width`; only pairs can wrap.
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
fn pane_title(source: &ReviewSource) -> String {
    match source {
        ReviewSource::WorkingTree
        | ReviewSource::Commit { .. }
        | ReviewSource::Walkthrough { .. } => "Diff".to_owned(),
        ReviewSource::Range { oldest, newest } => {
            let short = |oid: &str| oid.get(..7).unwrap_or(oid).to_owned();
            format!("Diff {}..{}", short(oldest), short(newest))
        }
        ReviewSource::Pr { number } => format!("PR #{number}"),
        ReviewSource::Against { .. } => format!("Diff {}", source.label()),
    }
}

/// The file sidebar's surface. A step away from the diff pane's surface is
/// what tells the two panes apart, so neither needs a border drawn: the diff
/// is the lit card, the file list lies flat on the app background.
fn sidebar_bg(theme: &Theme) -> Color {
    theme.bg
}

/// A pane's name as a plain heading row over its own surface, accented when
/// the pane holds focus.
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

/// The `(added, deleted)` line counts and the `(viewed, total)` file counts
/// each sidebar group covers, built once per frame from one pass over the
/// model: a header row knows its own name, not its members. A directory
/// covers every file beneath it at any depth; a section covers the files its
/// bucket holds. Only the layout on screen is walked. The review layout's own
/// buckets carry no `_viewed` entry: their name already says whether they
/// hold viewed files, so the header draws no rail from it.
#[derive(Default)]
struct GroupStat {
    dirs: HashMap<String, (usize, usize)>,
    sections: HashMap<Bucket, (usize, usize)>,
    dirs_viewed: HashMap<String, (usize, usize)>,
    sections_viewed: HashMap<Bucket, (usize, usize)>,
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
            let tally_viewed = |slot: &mut (usize, usize)| {
                slot.0 += usize::from(viewed);
                slot.1 += 1;
            };
            match diff.layout {
                FileLayout::Kinds => {
                    let bucket = Bucket::Kind(diff.kind_of(&file.path));
                    tally(stat.sections.entry(bucket).or_default());
                    tally_viewed(stat.sections_viewed.entry(bucket).or_default());
                }
                FileLayout::Review => {
                    let bucket = if viewed {
                        Bucket::Viewed
                    } else {
                        Bucket::ToReview
                    };
                    tally(stat.sections.entry(bucket).or_default());
                }
                // a stop row stands for a span of one file, so it carries no
                // group total
                FileLayout::Walkthrough => {}
                FileLayout::Tree | FileLayout::List => {
                    for (at, _) in file.path.match_indices('/') {
                        let dir = &file.path[..at];
                        tally(stat.dirs.entry(dir.to_owned()).or_default());
                        tally_viewed(stat.dirs_viewed.entry(dir.to_owned()).or_default());
                    }
                }
            }
        }
        stat
    }
}

/// Push `tail` against the row's right edge, gap-padded, when what is already
/// in `spans` leaves room for it. A row too narrow keeps its name and marks and
/// loses the tail, which is the part a reader can do without.
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

/// A sidebar row's background: the list surface, or the cursor band over it for
/// the row under the cursor.
fn sidebar_row_bg(theme: &Theme, on_cursor: bool, focused: bool) -> Color {
    if on_cursor {
        cursor_band(theme, sidebar_bg(theme), focused)
    } else {
        sidebar_bg(theme)
    }
}

/// A row's viewed signal for the lead cell: solid once everything it stands
/// for is viewed, muted while part of it still is, absent otherwise. A file
/// only ever carries `None` or `Done`; a directory or kind bucket carries
/// `Partial` for what a folded group can't otherwise say without unfolding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewedRail {
    None,
    Partial,
    Done,
}

impl ViewedRail {
    fn of_group(counts: Option<(usize, usize)>) -> Self {
        match counts {
            None | Some((0, _)) => Self::None,
            Some((viewed, total)) if viewed >= total => Self::Done,
            Some(_) => Self::Partial,
        }
    }
}

/// How far a partial rail leans from the row's own background toward the
/// viewed colour, muted enough to read as unfinished next to a solid one.
const PARTIAL_RAIL: u16 = 50;

/// Sidebar leading cells: the cursor `▌` marker plus the tree indent for
/// `depth`. Shared by dir and file rows so columns line up. The cursor always
/// wins the cell: a reader scans a run of viewed rows from a glance away, but
/// the row under the keyboard has to stay unmistakable up close, so its own
/// rail waits until the cursor moves off it.
fn tree_lead(
    theme: &Theme,
    depth: usize,
    bg: Color,
    on_cursor: bool,
    rail: ViewedRail,
) -> Span<'static> {
    let (marker, color) = if on_cursor {
        ("▌", theme.accent)
    } else {
        match rail {
            ViewedRail::None => (" ", theme.accent),
            ViewedRail::Partial => ("▌", crate::theme::blend(bg, theme.added, PARTIAL_RAIL)),
            ViewedRail::Done => ("▌", theme.added),
        }
    };
    Span::styled(
        format!("{marker}{}", " ".repeat(depth * 2)),
        Style::new().fg(color).bg(bg),
    )
}

/// A directory row: indent, fold arrow, the dim directory name, and the
/// diffstat of everything beneath it. `rail` says how much of it is viewed,
/// so a fully or partly reviewed directory reads that way while still folded.
fn sidebar_dir_line(
    rc: &TreeRowCtx<'_>,
    name: &str,
    folded: bool,
    stat: (usize, usize),
    rail: ViewedRail,
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
        tree_lead(theme, depth, bg, on_cursor, rail),
        Span::styled(arrow.to_owned(), Style::new().fg(theme.dim).bg(bg)),
    ];
    // dir names are never clipped, so the highlight maps straight onto them
    spans.extend(super::highlight_spans(name, name_style, search, theme));
    let tail = diffstat_spans(theme, stat.0, stat.1, bg);
    push_right(&mut spans, tail, width, bg);
    pad_line(spans, bg, width)
}

/// A section header row: `group_header_line` with the bucket's own diffstat
/// as its tail and `rail` for how much of the bucket is viewed. The review
/// layout's own buckets (`To review`, `Viewed`) already say that in their
/// name, so their caller passes `ViewedRail::None` rather than double it.
fn sidebar_section_line(
    rc: &TreeRowCtx<'_>,
    bucket: Bucket,
    count: usize,
    stat: (usize, usize),
    folded: bool,
    rail: ViewedRail,
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
            rail,
        },
        bucket.label(),
        count,
        folded,
        tail,
    )
}

/// A file row: a viewed rail in the lead cell, status glyph (colored),
/// basename, then the viewed and comment-count marks and the `+A -B`
/// diffstat. The diffstat is dropped first when the sidebar is too narrow to
/// keep the name and marks legible.
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
    let glyph = file.status.glyph();
    let rail = if viewed {
        ViewedRail::Done
    } else {
        ViewedRail::None
    };
    let mut spans = vec![
        tree_lead(theme, depth, bg, on_cursor, rail),
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
    // visible part; a path row (with a `/`) dims its parents and front-elides,
    // so its tail (the basename, the file's identity) stays in view and reads
    // as the name it is
    let parent = name.rfind('/').map_or(0, |at| at + 1);
    let highlighted = super::highlight_spans_split(name, parent, dim, name_style, search, theme);
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
    // fits after the name and marks: name + marks stay legible first
    let (added, deleted) = file.diffstat();
    push_right(
        &mut spans,
        diffstat_spans(theme, added, deleted, bg),
        width,
        bg,
    );
    pad_line(spans, bg, width)
}

/// Clip a name's already-styled `spans` to `room` cells, preserving each span's
/// style (so a search highlight survives on the visible cells). `front` elides
/// from the left with a leading `…` (for flat-list paths, keeping the tail
/// basename in view), otherwise from the right with a trailing `…`. The ellipsis
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

/// Terminal rows `row` occupies at `width`; only diff lines can wrap.
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
    search: &[(std::ops::Range<usize>, bool)],
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
                search,
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

/// Whether `line` falls inside any comment's anchored range for `file_path`:
/// drives the GitHub-style highlight marking a multi-line comment's scope.
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
        Style::new().fg(theme.accent).bg(bg),
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
    ctx: &RenderCtx<'_>,
    diff: &DiffView,
    comment: &Comment,
    line: usize,
    outdated: bool,
    width: u16,
    state: RowState,
) -> Line<'static> {
    let theme = ctx.theme;
    // a solid left bar in the comment's status color turns the block into a
    // distinct card that stands out against the diff lines around it
    let (status_label, accent) = match comment.status {
        CommentStatus::Open => ("open", theme.warn_fg),
        CommentStatus::Replied => ("replied", theme.accent),
        CommentStatus::Resolved => ("resolved", theme.dim),
    };
    let (bg, bar) = card_frame(theme, state.selected, state.focused, accent);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let fg = Style::new().fg(theme.fg).bg(bg);
    let blocks = crate::app::blocks_of(&diff.figures, &comment.id);
    let unresolved = diff.unresolved_anchors.get(&comment.id).copied();
    let lines = comment_display(comment, width, Some(ctx.highlighter), blocks, unresolved);
    let Some(part) = lines.get(line) else {
        return Line::default();
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
                Span::styled(comment.author.clone(), Style::new().fg(theme.purple).bg(bg)),
                Span::styled(" · ".to_owned(), dim),
                Span::styled(status_label.to_owned(), Style::new().fg(accent).bg(bg)),
            ]);
            if outdated {
                spans.push(Span::styled(
                    " · outdated".to_owned(),
                    Style::new().fg(theme.warn_fg).bg(bg),
                ));
            }
            // only the worker can tell an anchor that is gone from one that
            // has not been read yet, so the answer comes from its map
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
            return if state.selected {
                super::fill_row(drawn.clone(), bg, width)
            } else {
                drawn.clone()
            };
        }
        CommentLine::Reply {
            author,
            spans: runs,
            first,
        } => {
            let mut spans = vec![bar];
            if *first {
                spans.push(Span::styled(
                    format!("└ {author}: "),
                    Style::new().fg(theme.purple).bg(bg),
                ));
            } else {
                spans.push(Span::styled("  ".to_owned(), fg));
            }
            spans.extend(runs.iter().map(|run| md_span(run, fg, theme)));
            spans
        }
        CommentLine::Footer => vec![Span::styled(
            "  ▌".to_owned(),
            Style::new().fg(accent).bg(bg),
        )],
    };
    pad_line(spans, bg, width)
}

/// The walkthrough's own summary as one card: a plain "Summary" header (no
/// status, no author line, since nothing threads on it), its body, and any
/// figure it draws through the same figure cache a comment's card reads.
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
            return if state.selected {
                super::fill_row(drawn.clone(), bg, width)
            } else {
                drawn.clone()
            };
        }
        // the summary carries no anchor of its own, so nothing ever resolves
        // it and nothing ever answers it directly
        CommentLine::Note(_) | CommentLine::Reply { .. } => return Line::default(),
        CommentLine::Footer => vec![Span::styled(
            "  ▌".to_owned(),
            Style::new().fg(theme.accent).bg(bg),
        )],
    };
    pad_line(spans, bg, width)
}

/// The open composer, drawn as the card it is about to become: same bar, same
/// wrap, with the caret shown as a reversed cell so the writer sees where the
/// next character lands.
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
        ComposerKind::Edit { .. } => "editing".to_owned(),
    }
}

/// Map a markdown run's flags onto `base` (the body foreground over the card
/// background). Recoloring flags (code, link, muted) win over the base fg.
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

    /// The human and the agent never move, so the reader looks for those two
    /// colours first; the golden-angle step never lands on either for anyone
    /// else, and the same order always reads the same colour.
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

    /// Several reviewers stepping the golden angle from their first
    /// appearance read as visibly distinct hues, the separation a hash could
    /// only promise by chance, and none of them ever lands on the accent or
    /// purple the human and the agent keep.
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

    /// A titled comment is a walkthrough stop. In the list the title is the
    /// summary; the body stays in the card under its span, or ten stops of
    /// four bullets each turn the pane into a wall.
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
        // the body still draws in the pane's own card; only the list column
        // must leave it out, so read the screen from the pane's left edge
        // by column, since a row carrying box-drawing glyphs has more bytes
        // than columns and a byte slice into one lands mid-character
        let pane_start = screen
            .lines()
            .find_map(|row| row.find("Comments (").map(|at| row[..at].chars().count()))
            .expect("the comments pane heading");
        let pane: String = screen
            .lines()
            .map(|row| row.chars().skip(pane_start).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        // the pane is narrow, so the title is elided; the head of it is enough
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

    /// Two files, two authors (one the reviewer), one resolved thread: enough
    /// to give every grouping something real to show.
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

    /// A pane busy enough to prove the redesign holds up: three files, six
    /// authors including one long handle, a body too long for its collapsed
    /// row, and two folded groups on screen at once, none of which the
    /// shorter fixtures above ever show together.
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
        // land the cursor on the reviewer's own comment so its card opens,
        // leaving its long-named neighbour to show collapsed, elided, dense
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

    /// A stop list is only readable when the reader knows whose order it is,
    /// so the pane heading carries the walkthrough's name instead of "Files".
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

    /// A pin that no longer resolves (a squash, a rebase, a gc) is a fact
    /// about the whole walkthrough, not any one stop, so it shows in the
    /// sidebar heading rather than only on the stops that needed the pin.
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

    /// The card is the walkthrough's whole surface in the pane: a header, the
    /// body's markdown, and a figure, under the span it explains. A stop with
    /// nothing to point at heads the file instead.
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

    /// An overview stop has no line to sit on, so its card is the whole view.
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

    /// A stop anchored inside a large synthetic file shows its span and
    /// nothing else of the file: the bug this layout exists to fix.
    #[test]
    fn a_stop_in_a_large_file_windows_to_its_span() {
        let fixture = crate::test_support::big_file_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The change", Some("big.txt:100"), "why line 100 changed")],
        );
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// A slide shows its region and nothing else, so banding the region would
    /// paint every code row on screen and read as a selection; the band is for
    /// a span inside what is shown, which a comment jump in a file layout gets.
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

    /// Commenting inside a slide leaves it unbanded: the card's rows shift the
    /// code around them, which a band keyed on row numbers would follow into
    /// colouring the whole slide.
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

    /// A slide's second card sits right under the first: nothing from
    /// another stop leaks in, and nothing gets cut off.
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

    /// A stop's `notes` become extra agent comments in its own region:
    /// publishing writes one comment per note, and the sidebar row counts
    /// the whole slide, not just the stop.
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

    /// A `mermaid` fence in any comment draws as a figure, not as its source:
    /// what a stop keeps from the boards it replaced, and every other comment
    /// gains.
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

    /// A figure's `GraphView` used to default a selection on `set_model`
    /// (nothing asked for one, and a card figure is a static picture, not
    /// something being navigated), so it drew one node bold and reversed.
    /// Clearing the selection after `set_model` means no node in a card
    /// figure ever renders reversed.
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

    /// The walkthrough layout with the stops resolved, the way the worker
    /// leaves them.
    fn walkthrough_app(fixture: &Fixture, stops: &[(&str, Option<&str>, &str)]) -> App {
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        crate::test_support::seat_walkthrough(&mut app, "How the answer moved", stops);
        app.open_walkthrough_diff("w1");
        // the anchors resolve off-thread; the render tests want them landed
        let Some(request) = app.pending_walkthrough.take() else {
            return app;
        };
        let root = app.review.repo_root.clone();
        let read = diffler_core::review::Review::compute_walkthrough_files(
            &root,
            request.read_rev.as_deref(),
            &request.files,
        );
        app.handle(crate::event::AppEvent::WalkthroughAnchors {
            contents: read.contents,
            pin_broken: read.pin_broken,
            token: request.token,
        });
        app
    }

    /// `draw_pane` runs every frame; a stop anchored outside the diff forces
    /// `DiffView::model_with_context` to merge in a context file. That merge
    /// is the clone `crate::app::merge_count` counts, and a render must not
    /// trigger a fresh one: `ensure_rows` already cached it.
    #[test]
    fn draw_pane_reads_the_cached_merged_model_instead_of_rebuilding_it() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(&fixture, &[("Notes", Some("notes.txt:1"), "why alpha")]);
        // the first render settles `walkthrough_built` and enrichment, which
        // legitimately trigger a rebuild of their own; only renders after
        // that are the steady state a per-frame rebuild bug would show up in
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

    /// The summary slide renders as one card, with no diff rows beneath it.
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

    /// The selected card has to dim when the pane loses focus, exactly as a
    /// selected file row does, or there is no telling which pane the keys go to.
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

    /// The sidebar lights matches with the same two search colours every other
    /// pane uses: the active card's stronger, the rest plain.
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
            line: 2,
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
        // textual engine: it word-diffs the `41`→`42` literal so the emphasis
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

    /// A group's diffstat is the diff it holds, so a header says how big the
    /// directory or kind is without unfolding it.
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

    /// The review layout groups by viewed state, so its headers say how much
    /// diff is still to read.
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
        let spans = super::sidebar_dir_line(&rc, "src", false, (3, 1), super::ViewedRail::None);
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

    /// A group where every file is viewed leads with the same solid colour a
    /// viewed file itself does.
    #[test]
    fn a_fully_viewed_group_leads_with_the_viewed_colour() {
        let theme = Theme::github_dark();
        let rc = super::TreeRowCtx {
            theme: &theme,
            depth: 0,
            width: 40,
            on_cursor: false,
            focused: true,
            search: &[],
        };
        let spans = super::sidebar_dir_line(&rc, "src", true, (3, 1), super::ViewedRail::Done);
        assert_eq!(
            spans.spans[0].style.fg,
            Some(theme.added),
            "done reuses the file row's own viewed colour: {spans:?}"
        );
    }

    /// A group where only some files are viewed leads with a muted version of
    /// the viewed colour, readable as unfinished next to a fully done one.
    #[test]
    fn a_partly_viewed_group_leads_with_a_muted_colour() {
        let theme = Theme::github_dark();
        let rc = super::TreeRowCtx {
            theme: &theme,
            depth: 0,
            width: 40,
            on_cursor: false,
            focused: true,
            search: &[],
        };
        let spans = super::sidebar_dir_line(&rc, "src", true, (3, 1), super::ViewedRail::Partial);
        let fg = spans.spans[0]
            .style
            .fg
            .expect("a partial group still paints a rail");
        assert_ne!(
            fg, theme.added,
            "partial reads lighter than done: {spans:?}"
        );
        assert_ne!(
            fg, theme.accent,
            "partial never reads as the cursor: {spans:?}"
        );
    }

    /// The row under the keyboard always shows the cursor bar, even when it
    /// is itself a fully viewed group: the cursor has to stay unmistakable.
    #[test]
    fn the_cursor_bar_wins_over_a_viewed_rail() {
        let theme = Theme::github_dark();
        let rc = super::TreeRowCtx {
            theme: &theme,
            depth: 0,
            width: 40,
            on_cursor: true,
            focused: true,
            search: &[],
        };
        let spans = super::sidebar_dir_line(&rc, "src", true, (3, 1), super::ViewedRail::Done);
        assert_eq!(
            spans.spans[0].style.fg,
            Some(theme.accent),
            "the cursor colour wins over a done rail: {spans:?}"
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
        // the visual range repaints rows the bare cursor leaves alone
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
        app.handle(key('m'));
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("viewed 1/3 files"), "{content}");
    }

    #[test]
    fn viewed_walk_advances_the_selection() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('m'));
        app.handle(key('m'));
        // two files viewed; the sidebar cursor sits on the last unviewed
        // file, progress reads 2/3
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// Viewed files sort to the top of their group, so once a few are marked
    /// they stack into one run: their lead cells carry the rail, the file
    /// still to review below carries none.
    #[test]
    fn a_run_of_viewed_files_reads_as_one_stripe_in_the_sidebar() {
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

    /// A folder half viewed reads as partial while still folded, distinct
    /// from an untouched one and from one fully done, without opening it.
    #[test]
    fn a_partly_viewed_folded_folder_reads_as_partial() {
        let fixture = Fixture::new();
        fixture.write("src/a.rs", "one\n");
        fixture.write("src/b.rs", "one\n");
        fixture.commit_all("base");
        fixture.write("src/a.rs", "one\ntwo\n");
        fixture.write("src/b.rs", "one\ntwo\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let hash = app
            .review
            .model()
            .files
            .iter()
            .find(|f| f.path == "src/a.rs")
            .expect("src/a.rs in the diff")
            .content_hash();
        app.review
            .session_for_mut(&super::ReviewSource::WorkingTree)
            .mark_viewed("src/a.rs", &hash);
        let diff = app.diff.as_mut().expect("diff view");
        diff.folded_dirs.insert("src".to_owned());
        diff.invalidate();
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
    fn a_click_cannot_reach_past_an_open_composer() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "half written".chars() {
            app.handle(key(c));
        }
        render(&mut app);
        // the gesture that opens a fresh composer over this one, and the one
        // that would switch files out from under it
        let (x, y0, ..) = first_two_pane_lines(&app);
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(x, y0));
        app.handle(mouse_click(1, 3));
        let composer = app
            .diff
            .as_ref()
            .and_then(|d| d.composer.as_ref())
            .expect("the draft survives stray clicks");
        assert_eq!(composer.buffer, "half written");
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
