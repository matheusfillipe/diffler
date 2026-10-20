//! Diff pane of the diff screen: hunks, split view, scope crumbs and figures.

use std::collections::HashMap;

use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffLine, DiffModel, FileDiff};
use diffler_core::session::Session;
use diffler_core::source::ReviewSource;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::cards::{comment_row_line, composer_row_line, summary_row_line};
use super::sidebar::open_comment_count;
use super::{RenderCtx, RowState, pad_line, pane_heading};
use crate::app::composer::Composer;
use crate::app::rowsel::RowSelect;
use crate::app::walkthrough::Block as WalkthroughBlock;
use crate::app::{
    DiffRow, DiffView, FileHighlights, FileScope, Pane, RowCopy, SplitRow, SplitSide,
};
use crate::theme::Theme;
use crate::ui::color::slot_color;
use crate::ui::diff_render::{
    LineFlags, Mark, PairSelection, align_scroll, diff_line_height, file_gutter_width, fold_row,
    hunk_header, line_syntax, render_diff_line, render_split_pair, split_pair_height, syntax_row,
};
use crate::ui::{diffstat_spans, proportion_bar};

/// Rendered figure rows, keyed by the card's figure-cache key and block index.
pub(super) type FigureRaster = HashMap<(String, usize), Vec<Line<'static>>>;

pub(super) fn rasterize_figures(
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
                crate::ui::diff_render::figure_lines(
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
pub(super) fn patch_figure_copy_text(diff: &mut DiffView, rasters: &FigureRaster) {
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
struct SplitFileCtx<'a> {
    file: &'a FileDiff,
    highlights: Option<&'a FileHighlights>,
    gutter: usize,
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
        .map(|(range, symbol)| (range, Mark::Lens(slot_color(theme, symbol))))
        .collect();
    marks.extend(
        lens.labels(&file.path, on_old_side, number, &line.text)
            .into_iter()
            .map(|(range, digit, symbol)| (range, Mark::Label(digit, slot_color(theme, symbol)))),
    );
    marks
}

#[allow(clippy::too_many_lines)]
pub(super) fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
    diff: &mut DiffView,
) {
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
        let want = crate::ui::image_pane::draw_image_sides(
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
        let scroll = crate::ui::scroll_to_span(sel_start, sel_height, base, height, total);
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
            let first = lines.len();
            for (offset, line) in rendered.into_iter().enumerate() {
                let at = start + offset;
                if at < scroll || at >= scroll + height {
                    continue;
                }
                lines.push(line);
            }
            ctx.avatars.claim(first, lines.len() > first);
        }
        let top = top_row.and_then(|from| {
            split
                .iter()
                .skip(from)
                .find_map(|r| split_right_new_no(file, r))
        });
        render_scope_crumb(frame, crumb_area, theme, diff.scopes.get(&file.path), top);
        frame.render_widget(Paragraph::new(lines), rows_area);
        ctx.avatars.draw(frame, rows_area, 0);
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
    let (cur_start, mut cur_height) = match (starts.get(cursor), heights.get(cursor)) {
        (Some(s), Some(h)) => (*s, *h),
        _ => (0, 1),
    };
    // the comments pane walks whole cards and never the rows inside one, so
    // we keep its card in view as far as the pane allows
    if diff.focus == Pane::Comments
        && let Some(DiffRow::Comment { comment, .. }) = rows.get(cursor)
    {
        let card: usize = rows
            .iter()
            .zip(&heights)
            .skip(cursor)
            .take_while(
                |(row, _)| matches!(row, DiffRow::Comment { comment: c, .. } if c == comment),
            )
            .map(|(_, h)| h)
            .sum();
        cur_height = card.min(height.saturating_sub(2 * crate::ui::SCROLLOFF).max(1));
    }
    let base = diff.scroll_align.take().map_or(diff.scroll, |a| {
        align_scroll(a, cur_start, cur_height, height)
    });
    let scroll = crate::ui::scroll_to_span(cur_start, cur_height, base, height, total);
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
            rendered =
                crate::ui::band_referenced(rendered, referenced, index, theme, rows_area.width);
        }
        let first = lines.len();
        for (offset, line) in rendered.into_iter().enumerate() {
            let at = start + offset;
            if at < scroll || at >= scroll + height {
                continue;
            }
            lines.push(line);
            line_rows.push(Some(index));
        }
        ctx.avatars.claim(first, lines.len() > first);
    }
    diff.line_rows = line_rows;
    frame.render_widget(Paragraph::new(lines), rows_area);
    ctx.avatars.draw(frame, rows_area, 0);

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

#[cfg(test)]
mod tests {

    use crate::app::rowsel::RowSelect;
    use crate::app::{App, Pane};
    use crate::config::LoadedConfig;
    use crate::test_support::{
        Fixture, key, mouse_click, mouse_drag, mouse_right_click, mouse_scroll, render,
        standard_fixture,
    };
    use crate::theme::Theme;

    use crate::ui::diff::test_support::{
        cursor_to_added_line, diff_app, first_two_pane_lines, open_lib_diff, walkthrough_app,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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
    fn an_against_review_names_its_base_in_the_chip_and_the_pane_title() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_against_diff("main");
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
        for name in crate::theme::builtin_names() {
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
}
