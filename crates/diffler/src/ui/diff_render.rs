//! Pure diff-line renderers shared by the status screen's inline diffs and
//! the diff view: hunk headers, gutter line numbers, line-kind backgrounds,
//! intra-line emphasis ranges, and optional syntax foregrounds composited
//! per cell (syntax = fg, diff kind = bg, emphasis = stronger bg).

use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffLine, FileDiff, Hunk, LineKind};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Render one hunk as terminal lines: index 0 is the `@@` header, the rest
/// map 1:1 to `hunk.lines`. `selected` is an index into the returned vec
/// (0 = header) and paints that row with the cursor-line background.
pub fn render_hunk_lines(
    theme: &Theme,
    hunk: &Hunk,
    width: u16,
    selected: Option<usize>,
) -> Vec<Line<'static>> {
    let gutter = gutter_width(hunk);
    let mut lines = vec![hunk_header(theme, hunk, width, selected == Some(0))];
    lines.extend(hunk.lines.iter().enumerate().map(|(index, line)| {
        render_diff_line(
            theme,
            line,
            None,
            gutter,
            width,
            selected == Some(index + 1),
        )
    }));
    lines
}

/// Digits needed for the widest line number in the hunk, with a sane floor
/// so neighbouring hunks rarely disagree.
fn gutter_width(hunk: &Hunk) -> usize {
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

pub fn hunk_header(theme: &Theme, hunk: &Hunk, width: u16, selected: bool) -> Line<'static> {
    let bg = if selected {
        theme.cursor_line
    } else {
        theme.panel
    };
    let text = format!(" {}", hunk.header());
    let pad = (width as usize).saturating_sub(text.len());
    Line::from(vec![
        Span::styled(text, Style::new().fg(theme.dim).bg(bg)),
        Span::styled(" ".repeat(pad), Style::new().bg(bg)),
    ])
}

/// Render one diff line: gutter numbers, then the text composited from the
/// optional per-line syntax spans (fg) and the line's emphasis ranges (bg).
pub fn render_diff_line(
    theme: &Theme,
    line: &DiffLine,
    syntax: Option<&[StyledRange]>,
    gutter: usize,
    width: u16,
    selected: bool,
) -> Line<'static> {
    let line_bg = match line.kind {
        LineKind::Added => theme.add_line_bg,
        LineKind::Deleted => theme.del_line_bg,
        LineKind::Context => theme.bg,
    };
    let emph_bg = match line.kind {
        LineKind::Added => theme.add_emph_bg,
        LineKind::Deleted => theme.del_emph_bg,
        LineKind::Context => theme.bg,
    };
    let base_bg = if selected { theme.cursor_line } else { line_bg };

    let number = |n: Option<u32>| match n {
        Some(n) => format!("{n:>gutter$}"),
        None => " ".repeat(gutter),
    };
    let mut spans = vec![Span::styled(
        format!(" {} {} ", number(line.old_no), number(line.new_no)),
        Style::new().fg(theme.dim).bg(base_bg),
    )];
    spans.extend(composite_spans(theme, line, syntax, base_bg, emph_bg));

    let used: usize = spans.iter().map(Span::width).sum();
    let pad = (width as usize).saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::new().bg(base_bg)));
    }
    Line::from(spans)
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
    bounds.sort_unstable();
    bounds.dedup();

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
        let bg = if emphasized(start) { emph_bg } else { base_bg };
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
        let lines = render_hunk_lines(&theme, &sample_hunk(), 60, None);
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
            lines: vec![first_word],
        };
        let lines = render_hunk_lines(&theme, &hunk, 60, None);
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
        let plain = render_hunk_lines(&theme, &sample_hunk(), 60, None);
        let selected = render_hunk_lines(&theme, &sample_hunk(), 60, Some(2));
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
            lines: vec![bad],
        };
        let lines = render_hunk_lines(&theme, &hunk, 40, None);
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
            lines: vec![over],
        };
        let lines = render_hunk_lines(&theme, &hunk, 40, None);
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
        let rendered = render_diff_line(&theme, &added, Some(&syntax), 4, 60, false);
        let keyword: Vec<_> = rendered
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(Color::Rgb(255, 0, 0)))
            .collect();
        assert_eq!(keyword.len(), 1);
        assert_eq!(keyword[0].content.as_ref(), "let");
        assert!(keyword[0].style.add_modifier.contains(Modifier::BOLD));
        // "42" carries both the syntax fg and the emphasis bg
        let emphasized: Vec<_> = rendered
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(theme.add_emph_bg))
            .collect();
        assert_eq!(emphasized.len(), 1);
        assert_eq!(emphasized[0].content.as_ref(), "42");
        assert_eq!(emphasized[0].style.fg, Some(Color::Rgb(0, 255, 0)));
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
                    lines: vec![],
                },
                Hunk {
                    id: HunkId("b".into()),
                    old_start: 99990,
                    old_lines: 12,
                    new_start: 99990,
                    new_lines: 12,
                    lines: vec![],
                },
            ],
        };
        assert_eq!(file_gutter_width(&file), 6);
        let empty = FileDiff {
            hunks: vec![],
            ..file
        };
        assert_eq!(file_gutter_width(&empty), 4);
    }
}
