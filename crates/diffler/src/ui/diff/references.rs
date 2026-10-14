//! References sidebar of the diff screen.

use diffler_core::highlight::StyledRange;
use diffler_core::model::LineKind;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::cards::slot_color;
use super::sidebar::tree_lead;
use super::{RenderCtx, clip_spans, pad_line, pane_heading, sidebar_bg};
use crate::app::RefEntry;
use crate::app::{DiffView, FileHighlights, Pane};
use crate::theme::Theme;
use crate::ui::diff_render::{Mark, card_frame, syntax_row};

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
        let code_spans = crate::ui::diff_render::composite_spans(
            theme,
            &text,
            &[],
            Some(&spans),
            bg,
            bg,
            &marks,
        );
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
    let styled = crate::ui::highlight_spans_split(path, parent, dim, name, &[], theme);
    spans.extend(clip_spans(styled, room, true, dim));
    spans.push(Span::styled(count, dim));
    pad_line(spans, bg, width)
}

/// Right pane while a lens name is focused: its uses in diff order, grouped by file.
pub(super) fn draw_references(
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
    diff: &mut DiffView,
) {
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
    let color = slot_color(theme, slot);
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
    view.scroll = crate::ui::scroll_to_span(
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::Theme;
    use crate::ui::diff::test_support::line_text;

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
}
