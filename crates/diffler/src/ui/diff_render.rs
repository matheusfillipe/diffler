//! Pure diff-line renderers shared by the status screen's inline diffs and
//! the diff view: hunk headers, gutter line numbers, line-kind backgrounds,
//! intra-line emphasis ranges, and optional syntax foregrounds composited
//! per cell (syntax = fg, diff kind = bg, emphasis = stronger bg).

use std::ops::Range;

use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffLine, FileDiff, Hunk, LineKind};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::app::{ScrollAlign, SplitSide};
use crate::theme::Theme;

/// Scroll offset that places the cursor row at `align` within a `height`-line
/// viewport, given the row's visual start and height.
pub(super) fn align_scroll(
    align: ScrollAlign,
    cur_start: usize,
    cur_height: usize,
    height: usize,
) -> usize {
    match align {
        ScrollAlign::Center => (cur_start + cur_height / 2).saturating_sub(height / 2),
        ScrollAlign::Top => cur_start,
        ScrollAlign::Bottom => (cur_start + cur_height).saturating_sub(height),
    }
}

/// Per-file syntax for both diff sides — `(old, new)` — each indexed by line
/// number. The renderer picks the side per line via [`line_syntax`].
pub type SideSyntax<'a> = (&'a [Vec<StyledRange>], &'a [Vec<StyledRange>]);

/// Render one hunk as terminal lines: index 0 is the `@@` header, the rest
/// map 1:1 to `hunk.lines`. `selected` is an index into the returned vec
/// (0 = header) and paints that row with the cursor-line background.
///
/// `syntax` is the file's per-line syntax for both sides (`(old, new)`); when
/// present each line is highlighted exactly as the diff pane does, with syntax
/// foregrounds composited over the diff-kind background. `None` renders plain.
pub fn render_hunk_lines(
    theme: &Theme,
    hunk: &Hunk,
    syntax: Option<SideSyntax<'_>>,
    width: u16,
    selected: Option<usize>,
) -> Vec<Line<'static>> {
    let gutter = hunk_gutter_width(hunk);
    let mut lines = vec![hunk_header(theme, hunk, width, selected == Some(0))];
    lines.extend(hunk.lines.iter().enumerate().flat_map(|(index, line)| {
        let per_line = syntax.and_then(|(old, new)| line_syntax(old, new, line));
        render_diff_line(
            theme,
            line,
            per_line,
            gutter,
            width,
            selected == Some(index + 1),
            false,
            &[],
        )
    }));
    lines
}

/// The per-line syntax slice for `line`, picked from the file's cached
/// highlights: the old side for deletions, the new side for additions and
/// context, indexed by the line's number. `None` when the line has no number
/// on that side or the cache lacks it (e.g. a not-yet-highlighted file).
pub fn line_syntax<'a>(
    old: &'a [Vec<StyledRange>],
    new: &'a [Vec<StyledRange>],
    line: &DiffLine,
) -> Option<&'a [StyledRange]> {
    let (side, number) = match line.kind {
        LineKind::Deleted => (old, line.old_no),
        LineKind::Added | LineKind::Context => (new, line.new_no),
    };
    let index = usize::try_from(number?).ok()?.checked_sub(1)?;
    side.get(index).map(Vec::as_slice)
}

/// Digits needed for the widest line number in the hunk, with a sane floor
/// so neighbouring hunks rarely disagree.
pub fn hunk_gutter_width(hunk: &Hunk) -> usize {
    let max = (hunk.old_start + hunk.old_lines)
        .max(hunk.new_start + hunk.new_lines)
        .max(1);
    (max.ilog10() as usize + 1).max(4)
}

/// One gutter width for a whole file, so hunks in the continuous diff view
/// line up.
pub fn file_gutter_width(file: &FileDiff) -> usize {
    let max = file
        .hunks
        .iter()
        .map(|h| (h.old_start + h.old_lines).max(h.new_start + h.new_lines))
        .max()
        .unwrap_or(1)
        .max(1);
    (max.ilog10() as usize + 1).max(4)
}

/// GitHub-style section separator: a dim full-width band carrying git's
/// enclosing-section context (the `@@` line numbers are dropped as redundant
/// with the gutter). When git names no section the band alone reads as the
/// hunk boundary. Stays a navigable row so `{`/`}` hunk jumps land on it.
pub fn hunk_header(theme: &Theme, hunk: &Hunk, width: u16, selected: bool) -> Line<'static> {
    let bg = if selected {
        theme.cursor_line
    } else {
        theme.panel
    };
    let ranges = format!(
        "@@ -{},{} +{},{} @@",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
    );
    let text = if hunk.context.is_empty() {
        format!(" {ranges}")
    } else {
        format!(" {ranges} {}", hunk.context)
    };
    let pad = (width as usize).saturating_sub(text.chars().count());
    Line::from(vec![
        Span::styled(text, Style::new().fg(theme.dim).bg(bg)),
        Span::styled(" ".repeat(pad), Style::new().bg(bg)),
    ])
}

/// Columns a diff line's rail + gutter numbers occupy before the text.
fn prefix_width(gutter: usize) -> usize {
    1 + gutter * 2 + 2
}

/// Rows the greedy wrapper produces for characters of the given display
/// widths: the same loop as [`wrap_spans`], counting instead of building,
/// so height predictions can never drift from the render (a width-2 glyph
/// at a row boundary wastes a column that plain division would miscount).
fn greedy_rows(widths: impl Iterator<Item = usize>, budget: usize) -> usize {
    let mut rows = 1;
    let mut used = 0;
    for cw in widths {
        if used + cw > budget && used > 0 {
            rows += 1;
            used = 0;
        }
        used += cw;
    }
    rows
}

fn char_widths(text: &str) -> impl Iterator<Item = usize> + '_ {
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
}

/// Terminal rows a diff line needs at `width`: 1 plus one per wrapped
/// continuation. Agrees with [`render_diff_line`]'s wrapping by construction.
pub fn diff_line_height(line: &DiffLine, gutter: usize, width: u16) -> usize {
    let budget = (width as usize).saturating_sub(prefix_width(gutter)).max(1);
    greedy_rows(char_widths(&line.text), budget)
}

/// Render one diff line: gutter numbers, then the text composited from the
/// optional per-line syntax spans (fg) and the line's emphasis ranges (bg).
/// Text wider than the pane wraps onto continuation rows under a blank gutter.
// orthogonal styling inputs (gutter, selection, annotation, search ranges); a
// params struct would not read more clearly
#[allow(clippy::too_many_arguments)]
pub fn render_diff_line(
    theme: &Theme,
    line: &DiffLine,
    syntax: Option<&[StyledRange]>,
    gutter: usize,
    width: u16,
    selected: bool,
    annotated: bool,
    search: &[(Range<usize>, bool)],
) -> Vec<Line<'static>> {
    let (base_bg, emph_bg) = line_backgrounds(theme, line, selected, annotated);

    let number = |n: Option<u32>| match n {
        Some(n) => format!("{n:>gutter$}"),
        None => " ".repeat(gutter),
    };
    let prefix = |first: bool| {
        vec![
            Span::styled(
                rail(line),
                Style::new().fg(rail_color(theme, line)).bg(base_bg),
            ),
            Span::styled(
                if first {
                    format!("{} {} ", number(line.old_no), number(line.new_no))
                } else {
                    " ".repeat(gutter * 2 + 2)
                },
                Style::new().fg(theme.dim).bg(base_bg),
            ),
        ]
    };
    let content = composite_spans(theme, line, syntax, base_bg, emph_bg, search);
    let budget = (width as usize).saturating_sub(prefix_width(gutter)).max(1);
    wrap_spans(content, budget)
        .into_iter()
        .enumerate()
        .map(|(row, segment)| {
            let mut spans = prefix(row == 0);
            spans.extend(segment);
            let used: usize = spans.iter().map(Span::width).sum();
            let pad = (width as usize).saturating_sub(used);
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), Style::new().bg(base_bg)));
            }
            Line::from(spans)
        })
        .collect()
}

/// Split styled spans into rows of at most `budget` display columns, cutting
/// at character boundaries so every style survives the wrap. Always yields at
/// least one row.
fn wrap_spans(spans: Vec<Span<'static>>, budget: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let style = span.style;
        let mut chunk = String::new();
        for c in span.content.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0);
            if used + cw > budget && used > 0 {
                if !chunk.is_empty() {
                    current.push(Span::styled(std::mem::take(&mut chunk), style));
                }
                rows.push(std::mem::take(&mut current));
                used = 0;
            }
            chunk.push(c);
            used += cw;
        }
        if !chunk.is_empty() {
            current.push(Span::styled(chunk, style));
        }
    }
    rows.push(current);
    rows
}

/// The line (base) and emphasis backgrounds, given selection/annotation state.
fn line_backgrounds(
    theme: &Theme,
    line: &DiffLine,
    selected: bool,
    annotated: bool,
) -> (Color, Color) {
    let (line_bg, emph_bg) = match line.kind {
        LineKind::Added => (theme.add_line_bg, theme.add_emph_bg),
        LineKind::Deleted => (theme.del_line_bg, theme.del_emph_bg),
        LineKind::Context => (theme.bg, theme.bg),
    };
    let base_bg = if selected {
        theme.cursor_line
    } else if annotated {
        theme.annotated
    } else {
        line_bg
    };
    (base_bg, emph_bg)
}

fn rail(_line: &DiffLine) -> &'static str {
    " "
}

fn rail_color(theme: &Theme, line: &DiffLine) -> Color {
    match line.kind {
        LineKind::Added => theme.added,
        LineKind::Deleted => theme.error_fg,
        LineKind::Context => theme.dim,
    }
}

/// One side of a side-by-side row: the line and its per-line syntax, or `None`
/// for a column with no counterpart (a lone deletion's right, a lone
/// addition's left).
pub type SplitCell<'a> = Option<(&'a DiffLine, Option<&'a [StyledRange]>, bool)>;

/// Render one side-by-side row: the old line in the left column, the new line
/// in the right, divided by a separator. Each column shows a single gutter
/// number (old on the left, new on the right) and the same composited text the
/// unified view draws. `sel_left`/`sel_right` paint a column with the
/// cursor-line background.
/// Columns of a split cell taken by its rail + one gutter number.
fn split_prefix_width(gutter: usize) -> usize {
    1 + gutter + 1
}

/// Terminal rows a side-by-side row needs at `width`: the taller of its two
/// wrapped sides. Must agree with [`render_split_pair`]'s wrapping.
pub fn split_pair_height(
    left: Option<&DiffLine>,
    right: Option<&DiffLine>,
    gutter: usize,
    width: u16,
) -> usize {
    let total = width as usize;
    let left_w = total.saturating_sub(1) / 2;
    let right_w = total.saturating_sub(1 + left_w);
    let side = |line: Option<&DiffLine>, col_width: usize| {
        let Some(line) = line else { return 1 };
        let budget = col_width.saturating_sub(split_prefix_width(gutter)).max(1);
        greedy_rows(char_widths(&line.text), budget)
    };
    side(left, left_w).max(side(right, right_w))
}

pub fn render_split_pair(
    theme: &Theme,
    left: SplitCell<'_>,
    right: SplitCell<'_>,
    gutter: usize,
    width: u16,
    sel_left: bool,
    sel_right: bool,
) -> Vec<Line<'static>> {
    let total = width as usize;
    let left_w = total.saturating_sub(1) / 2;
    let right_w = total.saturating_sub(1 + left_w);
    let mut left_rows = side_rows(theme, left, SplitSide::Left, gutter, left_w, sel_left);
    let mut right_rows = side_rows(theme, right, SplitSide::Right, gutter, right_w, sel_right);
    let rows = left_rows.len().max(right_rows.len());
    let filler = |cell: SplitCell<'_>, selected: bool, col_width: usize| {
        let bg = match cell {
            Some((line, ..)) => line_backgrounds(theme, line, selected, false).0,
            None if selected => theme.cursor_line,
            None => theme.bg,
        };
        vec![Span::styled(" ".repeat(col_width), Style::new().bg(bg))]
    };
    while left_rows.len() < rows {
        left_rows.push(filler(left, sel_left, left_w));
    }
    while right_rows.len() < rows {
        right_rows.push(filler(right, sel_right, right_w));
    }
    left_rows
        .into_iter()
        .zip(right_rows)
        .map(|(mut spans, right)| {
            spans.push(Span::styled("│", Style::new().fg(theme.dim).bg(theme.bg)));
            spans.extend(right);
            Line::from(spans)
        })
        .collect()
}

/// Render one column of a side-by-side row, wrapped and padded to
/// `col_width`; continuations get a blank gutter.
fn side_rows(
    theme: &Theme,
    cell: SplitCell<'_>,
    side: SplitSide,
    gutter: usize,
    col_width: usize,
    selected: bool,
) -> Vec<Vec<Span<'static>>> {
    let Some((line, syntax, annotated)) = cell else {
        let bg = if selected {
            theme.cursor_line
        } else {
            theme.bg
        };
        return vec![vec![Span::styled(
            " ".repeat(col_width),
            Style::new().bg(bg),
        )]];
    };
    let (base_bg, emph_bg) = line_backgrounds(theme, line, selected, annotated);
    let number = match side {
        SplitSide::Left => line.old_no,
        SplitSide::Right => line.new_no,
    };
    let number = match number {
        Some(n) => format!("{n:>gutter$}"),
        None => " ".repeat(gutter),
    };
    let prefix = |first: bool| {
        vec![
            Span::styled(
                rail(line),
                Style::new().fg(rail_color(theme, line)).bg(base_bg),
            ),
            Span::styled(
                if first {
                    format!("{number} ")
                } else {
                    " ".repeat(gutter + 1)
                },
                Style::new().fg(theme.dim).bg(base_bg),
            ),
        ]
    };
    let content = composite_spans(theme, line, syntax, base_bg, emph_bg, &[]);
    let budget = col_width.saturating_sub(split_prefix_width(gutter)).max(1);
    wrap_spans(content, budget)
        .into_iter()
        .enumerate()
        .map(|(row, segment)| {
            let mut spans = prefix(row == 0);
            spans.extend(segment);
            clip_pad(spans, col_width, base_bg)
        })
        .collect()
}

/// Clip a styled run to `width` display columns, padding the remainder with
/// the background so every column fills exactly.
fn clip_pad(spans: Vec<Span<'static>>, width: usize, bg: Color) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        if used >= width {
            break;
        }
        let w = span.width();
        if used + w <= width {
            used += w;
            out.push(span);
        } else {
            // clip by display width, not char count, so a wide (CJK/emoji)
            // glyph at the boundary can't overrun the column and shove the
            // neighbouring column off the pane
            let mut clipped = String::new();
            for ch in span.content.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if used + cw > width {
                    break;
                }
                clipped.push(ch);
                used += cw;
            }
            out.push(Span::styled(clipped, span.style));
            break;
        }
    }
    if used < width {
        out.push(Span::styled(" ".repeat(width - used), Style::new().bg(bg)));
    }
    out
}

/// Split the text at every syntax/emphasis range boundary and style each
/// segment: foreground from the syntax span covering it, background from
/// whether an emphasis range covers it. Byte offsets are snapped to char
/// boundaries defensively so a malformed range can never split a
/// multi-byte character.
fn composite_spans(
    theme: &Theme,
    line: &DiffLine,
    syntax: Option<&[StyledRange]>,
    base_bg: Color,
    emph_bg: Color,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let text = line.text.as_str();
    let snap = |index: usize| snap_to_boundary(text, index.min(text.len()));

    let mut bounds = vec![0, text.len()];
    for range in &line.emphasis {
        bounds.push(snap(range.start));
        bounds.push(snap(range.end));
    }
    for styled in syntax.unwrap_or_default() {
        bounds.push(snap(styled.range.start));
        bounds.push(snap(styled.range.end));
    }
    for (range, _) in search {
        bounds.push(snap(range.start));
        bounds.push(snap(range.end));
    }
    bounds.sort_unstable();
    bounds.dedup();

    // a search match outranks emphasis on the chars it covers
    let search_at = |at: usize| {
        search
            .iter()
            .find(|(range, _)| snap(range.start) <= at && at < snap(range.end))
            .map(|(_, current)| *current)
    };
    let emphasized = |at: usize| {
        line.emphasis
            .iter()
            .any(|range| snap(range.start) <= at && at < snap(range.end))
    };
    let syntax_at = |at: usize| {
        syntax?
            .iter()
            .find(|styled| snap(styled.range.start) <= at && at < snap(styled.range.end))
    };

    let mut spans = Vec::new();
    for pair in bounds.windows(2) {
        let &[start, end] = pair else { continue };
        let Some(segment) = text.get(start..end) else {
            continue;
        };
        if segment.is_empty() {
            continue;
        }
        let bg = match search_at(start) {
            Some(true) => theme.search_current,
            Some(false) => theme.search,
            None if emphasized(start) => emph_bg,
            None => base_bg,
        };
        let mut style = Style::new().fg(theme.fg).bg(bg);
        if let Some(styled) = syntax_at(start) {
            let (r, g, b) = styled.fg;
            style = style.fg(Color::Rgb(r, g, b));
            if styled.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if styled.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
        }
        spans.push(Span::styled(segment.to_owned(), style));
    }
    spans
}

fn snap_to_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use diffler_core::model::HunkId;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;

    use super::*;

    #[test]
    fn align_scroll_positions_the_cursor_row() {
        assert_eq!(align_scroll(ScrollAlign::Top, 20, 1, 10), 20);
        assert_eq!(align_scroll(ScrollAlign::Bottom, 20, 1, 10), 11);
        assert_eq!(align_scroll(ScrollAlign::Center, 20, 1, 10), 15);
        // clamps at the top of the buffer
        assert_eq!(align_scroll(ScrollAlign::Center, 1, 1, 10), 0);
    }

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
        DiffLine::new(kind, old, new, text.to_owned())
    }

    fn sample_hunk() -> Hunk {
        let mut deleted = line(LineKind::Deleted, Some(2), None, "    41");
        deleted.emphasis.push(5..6);
        let mut added = line(LineKind::Added, None, Some(2), "    42");
        added.emphasis.push(5..6);
        Hunk {
            id: HunkId("h".into()),
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            context: String::new(),
            lines: vec![
                line(LineKind::Context, Some(1), Some(1), "fn answer() {"),
                deleted,
                added,
                line(LineKind::Context, Some(3), Some(3), "}"),
            ],
        }
    }

    fn render(lines: &[Line<'static>]) -> Terminal<TestBackend> {
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new(lines.to_vec()), frame.area()))
            .expect("draw");
        terminal
    }

    #[test]
    fn hunk_renders_header_gutter_and_emphasis() {
        let theme = Theme::github_dark();
        let lines = render_hunk_lines(&theme, &sample_hunk(), None, 60, None);
        assert_eq!(lines.len(), 5, "header + 4 diff lines");
        let has_bg = |line: &Line<'_>, bg| line.spans.iter().any(|s| s.style.bg == Some(bg));
        assert!(has_bg(&lines[2], theme.del_emph_bg), "deleted emphasis bg");
        assert!(has_bg(&lines[3], theme.add_emph_bg), "added emphasis bg");
        insta::assert_snapshot!(render(&lines).backend());
    }

    #[test]
    fn emphasis_starting_at_byte_zero_is_rendered() {
        let theme = Theme::github_dark();
        let mut first_word = line(LineKind::Added, None, Some(1), "boo bar");
        first_word.emphasis.push(0..3);
        let hunk = Hunk {
            id: HunkId("h".into()),
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 1,
            context: String::new(),
            lines: vec![first_word],
        };
        let lines = render_hunk_lines(&theme, &hunk, None, 60, None);
        let emphasized: String = lines[1]
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(theme.add_emph_bg))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(emphasized, "boo");
    }

    #[test]
    fn selected_row_gets_cursor_background() {
        let theme = Theme::github_dark();
        let plain = render_hunk_lines(&theme, &sample_hunk(), None, 60, None);
        let selected = render_hunk_lines(&theme, &sample_hunk(), None, 60, Some(2));
        assert_eq!(plain[1], selected[1], "unselected rows unchanged");
        assert_ne!(plain[2], selected[2], "selected row repainted");
    }

    #[test]
    fn emphasis_mid_multibyte_char_does_not_panic_or_split() {
        let theme = Theme::github_dark();
        // "é" is 2 bytes: a range ending inside it must snap, not panic
        let mut bad = line(LineKind::Added, None, Some(1), "héllo");
        bad.emphasis = vec![1..2, 3..4];
        let hunk = Hunk {
            id: HunkId("h".into()),
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 1,
            context: String::new(),
            lines: vec![bad],
        };
        let lines = render_hunk_lines(&theme, &hunk, None, 40, None);
        let text: String = lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.contains("héllo"));
    }

    #[test]
    fn emphasis_range_past_end_is_clamped() {
        let theme = Theme::github_dark();
        let mut over = line(LineKind::Deleted, Some(1), None, "ab");
        over.emphasis.push(1..99);
        let hunk = Hunk {
            id: HunkId("h".into()),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 0,
            context: String::new(),
            lines: vec![over],
        };
        let lines = render_hunk_lines(&theme, &hunk, None, 40, None);
        let text: String = lines[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.contains("ab"));
    }

    #[test]
    fn syntax_spans_color_the_foreground_and_emphasis_keeps_the_bg() {
        let theme = Theme::github_dark();
        let mut added = line(LineKind::Added, None, Some(1), "let x = 42;");
        added.emphasis.push(8..10);
        let syntax = vec![
            StyledRange {
                range: 0..3,
                fg: (255, 0, 0),
                bold: true,
                italic: false,
            },
            StyledRange {
                range: 4..11,
                fg: (0, 255, 0),
                bold: false,
                italic: false,
            },
        ];
        let rendered = render_diff_line(&theme, &added, Some(&syntax), 4, 60, false, false, &[]);
        assert_eq!(rendered.len(), 1, "short line stays one row");
        let keyword: Vec<_> = rendered[0]
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(Color::Rgb(255, 0, 0)))
            .collect();
        assert_eq!(keyword.len(), 1);
        assert_eq!(keyword[0].content.as_ref(), "let");
        assert!(keyword[0].style.add_modifier.contains(Modifier::BOLD));
        // "42" carries both the syntax fg and the emphasis bg
        let emphasized: Vec<_> = rendered[0]
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(theme.add_emph_bg))
            .collect();
        assert_eq!(emphasized.len(), 1);
        assert_eq!(emphasized[0].content.as_ref(), "42");
        assert_eq!(emphasized[0].style.fg, Some(Color::Rgb(0, 255, 0)));
    }

    #[test]
    fn every_added_line_keeps_the_full_background() {
        // reindents/moves included: there is no in-between render state
        let theme = Theme::github_dark();
        let reindented = line(LineKind::Added, None, Some(2), "    <Form>");
        let rendered = render_diff_line(&theme, &reindented, None, 3, 60, false, false, &[]);
        assert!(
            rendered[0]
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme.add_line_bg)),
            "added lines always carry the added background"
        );
    }

    #[test]
    fn long_lines_wrap_under_a_blank_gutter_and_keep_their_text() {
        let theme = Theme::github_dark();
        let text = "x".repeat(100);
        let long = line(LineKind::Added, None, Some(2), &text);
        let width = 40u16;
        let rendered = render_diff_line(&theme, &long, None, 4, width, false, false, &[]);
        assert_eq!(rendered.len(), diff_line_height(&long, 4, width));
        assert!(rendered.len() > 1, "100 columns cannot fit in 40");
        for row in &rendered {
            let total: usize = row.spans.iter().map(Span::width).sum();
            assert_eq!(total, width as usize, "every row fills the pane exactly");
        }
        let joined: String = rendered
            .iter()
            .flat_map(|row| row.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert_eq!(
            joined.chars().filter(|c| *c == 'x').count(),
            100,
            "no character is lost to the wrap"
        );
        let continuation = &rendered[1].spans[1];
        assert!(
            continuation.content.chars().all(|c| c == ' '),
            "continuations get a blank gutter: {continuation:?}"
        );
    }

    #[test]
    fn split_pair_height_is_the_taller_wrapped_side() {
        let long = line(LineKind::Added, None, Some(2), &"y".repeat(60));
        let short = line(LineKind::Deleted, Some(2), None, "one");
        assert_eq!(split_pair_height(None, None, 4, 80), 1);
        // right column: 80 - 1 (divider) - 39 (left) = 40 wide, minus the
        // rail + gutter prefix of 6 = 34 text columns
        assert_eq!(
            split_pair_height(Some(&short), Some(&long), 4, 80),
            60usize.div_ceil(34)
        );
    }

    #[test]
    fn height_prediction_matches_the_render_for_wide_glyphs() {
        // width-2 glyphs waste a column at odd budgets; plain division
        // miscounts what the greedy wrapper actually emits
        let theme = Theme::github_dark();
        let cjk = line(LineKind::Added, None, Some(2), &"あ".repeat(5));
        for width in [14u16, 15, 16, 40] {
            let rendered = render_diff_line(&theme, &cjk, None, 4, width, false, false, &[]);
            assert_eq!(
                rendered.len(),
                diff_line_height(&cjk, 4, width),
                "width {width}"
            );
        }
        let long = line(LineKind::Added, None, Some(2), &"あ".repeat(441));
        let rendered = render_diff_line(&theme, &long, None, 4, 100, false, false, &[]);
        assert_eq!(rendered.len(), diff_line_height(&long, 4, 100));
    }

    #[test]
    fn file_gutter_width_spans_all_hunks() {
        let file = FileDiff {
            path: "f.rs".into(),
            old_path: None,
            status: diffler_core::model::FileStatus::Modified,
            binary: false,
            old_text: None,
            new_text: None,
            hunks: vec![
                Hunk {
                    id: HunkId("a".into()),
                    old_start: 1,
                    old_lines: 3,
                    new_start: 1,
                    new_lines: 3,
                    context: String::new(),
                    lines: vec![],
                },
                Hunk {
                    id: HunkId("b".into()),
                    old_start: 99990,
                    old_lines: 12,
                    new_start: 99990,
                    new_lines: 12,
                    context: String::new(),
                    lines: vec![],
                },
            ],
            hashes: diffler_core::model::HashCache::default(),
        };
        assert_eq!(file_gutter_width(&file), 6);
        let empty = FileDiff {
            hunks: vec![],
            hashes: diffler_core::model::HashCache::default(),
            ..file
        };
        assert_eq!(file_gutter_width(&empty), 4);
    }

    #[test]
    fn clip_pad_clips_by_display_width_not_char_count() {
        // five double-width glyphs are ten columns; clipping to six must keep
        // three glyphs, never overrun the column and shove a neighbour aside
        let spans = vec![Span::raw("一二三四五")];
        let out = clip_pad(spans, 6, Color::Reset);
        let width: usize = out.iter().map(Span::width).sum();
        assert_eq!(width, 6);
    }

    #[test]
    fn clip_pad_pads_a_short_run_to_the_column_width() {
        let out = clip_pad(vec![Span::raw("ab")], 6, Color::Reset);
        let width: usize = out.iter().map(Span::width).sum();
        assert_eq!(width, 6);
    }
}
