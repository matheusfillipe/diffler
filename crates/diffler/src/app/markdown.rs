//! Minimal `CommonMark` rendering for comment and reply bodies. Parses to
//! theme-independent styled runs so the pure app layer stays free of ratatui
//! and the theme; [`crate::ui`] maps the flags to concrete styles at draw time.
//! Raw HTML is dropped rather than shown, so a stray tag in an agent reply does
//! not leak into the card.

use diffler_core::highlight::{Highlighter, StyledRange};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

/// A styled text run with no line breaks. Flags compose (bold + italic), so
/// nested emphasis survives; `code`/`link`/`muted` additionally recolor. `fg`
/// carries a fenced code block's syntax color, overriding the flag styling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // flags compose; an enum cannot
pub struct MdSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: bool,
    pub muted: bool,
    /// Laid out already (a table row): [`wrap`] leaves the line alone, since
    /// re-splitting on spaces would collapse the columns.
    pub pre: bool,
    pub fg: Option<(u8, u8, u8)>,
}

impl MdSpan {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    fn width(&self) -> usize {
        self.text.width()
    }
}

#[derive(Default)]
struct Flags {
    bold: usize,
    italic: usize,
    strike: usize,
    link: bool,
    heading: bool,
}

/// A table under construction: cells collect as runs and lay out once the
/// whole table is known, since a column is only as wide as its widest cell.
#[derive(Default)]
struct Table {
    head: Vec<Vec<MdSpan>>,
    rows: Vec<Vec<Vec<MdSpan>>>,
    cell: Vec<MdSpan>,
    in_head: bool,
    row: Vec<Vec<MdSpan>>,
}

/// Columns narrower than this hold nothing readable, so the table is listed
/// row by row instead.
const MIN_COLUMN: usize = 8;
/// Blank columns between cells.
const COLUMN_GAP: usize = 2;

/// Parse markdown into logical lines of styled runs (unwrapped). Line breaks,
/// block boundaries, list items, and code-block lines each start a new logical
/// line; a comment's own newlines are kept (GitHub renders them).
#[allow(clippy::too_many_lines)] // one arm per markdown event; a flat match reads best
pub fn parse(src: &str, highlighter: Option<&Highlighter>, width: usize) -> Vec<Vec<MdSpan>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_TABLES);
    let mut lines: Vec<Vec<MdSpan>> = Vec::new();
    let mut line: Vec<MdSpan> = Vec::new();
    let mut flags = Flags::default();
    let mut list_depth: usize = 0;
    let mut code_block: Option<String> = None;
    let mut code_lang: Option<String> = None;
    let mut link_url: Option<String> = None;
    // a list item's own paragraph must not flush the bullet onto its own line
    let mut item_paragraph = false;
    let mut table: Option<Table> = None;
    // the next number each open ordered list will stamp on its item
    let mut ordered: Vec<Option<u64>> = Vec::new();
    let mut quote_depth: usize = 0;

    let flush = |line: &mut Vec<MdSpan>, lines: &mut Vec<Vec<MdSpan>>, depth: usize| {
        if !line.is_empty() {
            lines.push(quoted(std::mem::take(line), depth));
        }
    };

    for event in Parser::new_ext(src, opts) {
        match event {
            Event::Start(Tag::Strong) => flags.bold += 1,
            Event::End(TagEnd::Strong) => flags.bold = flags.bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => flags.italic += 1,
            Event::End(TagEnd::Emphasis) => flags.italic = flags.italic.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => flags.strike += 1,
            Event::End(TagEnd::Strikethrough) => flags.strike = flags.strike.saturating_sub(1),
            Event::Start(Tag::Heading { .. }) => {
                flush(&mut line, &mut lines, quote_depth);
                flags.heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut line, &mut lines, quote_depth);
                flags.heading = false;
            }
            Event::Start(Tag::List(start)) => {
                list_depth += 1;
                ordered.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                ordered.pop();
            }
            Event::Start(Tag::Item) => {
                flush(&mut line, &mut lines, quote_depth);
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                let marker = match ordered.last_mut().and_then(Option::as_mut) {
                    Some(next) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    None => "• ".to_owned(),
                };
                line.push(MdSpan {
                    text: format!("{indent}{marker}"),
                    muted: true,
                    ..MdSpan::default()
                });
                item_paragraph = true;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                // a quote opening a list item keeps the marker it just pushed;
                // the paragraph inside the quote is the one that consumes it
                if !item_paragraph {
                    flush(&mut line, &mut lines, quote_depth);
                }
                quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut line, &mut lines, quote_depth);
                quote_depth = quote_depth.saturating_sub(1);
            }
            Event::Rule => {
                flush(&mut line, &mut lines, quote_depth);
                let across = width.saturating_sub(rail_width(quote_depth)).max(1);
                lines.push(quoted(
                    vec![MdSpan {
                        text: "─".repeat(across),
                        muted: true,
                        ..MdSpan::default()
                    }],
                    quote_depth,
                ));
            }
            Event::Start(Tag::Table(_)) => {
                flush(&mut line, &mut lines, quote_depth);
                table = Some(Table::default());
            }
            Event::Start(Tag::TableHead) => {
                if let Some(table) = table.as_mut() {
                    table.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if let Some(table) = table.as_mut() {
                    let row = std::mem::take(&mut table.row);
                    if table.in_head {
                        table.head = row;
                        table.in_head = false;
                    } else {
                        table.rows.push(row);
                    }
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(table) = table.as_mut() {
                    let mut cell = std::mem::take(&mut table.cell);
                    if table.in_head {
                        for span in &mut cell {
                            span.bold = true;
                        }
                    }
                    table.row.push(cell);
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = table.take() {
                    let across = width.saturating_sub(rail_width(quote_depth));
                    for row in table_lines(&table.head, &table.rows, across) {
                        lines.push(quoted(row, quote_depth));
                    }
                }
            }
            Event::End(TagEnd::Item) => {
                item_paragraph = false;
                flush(&mut line, &mut lines, quote_depth);
            }
            Event::Start(Tag::Paragraph) => {
                if item_paragraph {
                    item_paragraph = false;
                } else {
                    flush(&mut line, &mut lines, quote_depth);
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut line, &mut lines, quote_depth);
                code_block = Some(String::new());
                code_lang = match kind {
                    CodeBlockKind::Fenced(lang) if !lang.is_empty() => Some(lang.into_string()),
                    _ => None,
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(buf) = code_block.take() {
                    let body = buf.trim_end_matches('\n');
                    let colors = code_lang
                        .take()
                        .zip(highlighter)
                        .map(|(lang, hl)| hl.highlight_lang(&lang, body));
                    for (i, text) in body.split('\n').enumerate() {
                        let ranges = colors
                            .as_ref()
                            .and_then(|per_line| per_line.get(i))
                            .map_or(&[][..], Vec::as_slice);
                        lines.push(quoted(code_line_spans(text, ranges), quote_depth));
                    }
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                flags.link = true;
                link_url = Some(dest_url.into_string());
            }
            Event::End(TagEnd::Link) => {
                flags.link = false;
                if let Some(url) = link_url.take() {
                    // the suffix belongs beside the link text, which inside a
                    // table is the cell rather than the line being built
                    let shown: String = match table.as_ref() {
                        Some(table) => table.cell.iter().map(|s| s.text.as_str()).collect(),
                        None => line.iter().map(|s| s.text.as_str()).collect(),
                    };
                    if !url.is_empty() && !shown.contains(&url) {
                        push_span(
                            MdSpan {
                                text: format!(" ({url})"),
                                muted: true,
                                ..MdSpan::default()
                            },
                            table.as_mut(),
                            &mut line,
                        );
                    }
                }
            }
            Event::Text(text) => {
                if let Some(buf) = code_block.as_mut() {
                    buf.push_str(&text);
                } else {
                    let span = MdSpan {
                        text: text.into_string(),
                        bold: flags.bold > 0 || flags.heading,
                        italic: flags.italic > 0,
                        strike: flags.strike > 0,
                        link: flags.link,
                        ..MdSpan::default()
                    };
                    push_span(span, table.as_mut(), &mut line);
                }
            }
            Event::Code(text) => {
                let span = MdSpan {
                    text: text.into_string(),
                    code: true,
                    bold: flags.bold > 0 || flags.heading,
                    italic: flags.italic > 0,
                    link: flags.link,
                    ..MdSpan::default()
                };
                push_span(span, table.as_mut(), &mut line);
            }
            Event::TaskListMarker(checked) => line.push(MdSpan {
                text: if checked { "[x] " } else { "[ ] " }.to_owned(),
                muted: true,
                ..MdSpan::default()
            }),
            // a review comment's own line breaks are meaningful (GitHub renders
            // them), so a soft break starts a new line rather than a space
            Event::End(TagEnd::Paragraph) | Event::SoftBreak | Event::HardBreak => {
                flush(&mut line, &mut lines, quote_depth);
            }
            _ => {}
        }
    }
    flush(&mut line, &mut lines, quote_depth);
    lines
}

/// Route a run to the cell it belongs to, or to the line being built.
fn push_span(span: MdSpan, table: Option<&mut Table>, line: &mut Vec<MdSpan>) {
    match table {
        Some(table) => table.cell.push(span),
        None => line.push(span),
    }
}

/// Open a line with the rail of the quote it sits in. Every completed line
/// passes through here, so the rail reaches the ones nobody types by hand:
/// list items, code, tables, rules. The rail inherits the line's `pre` flag,
/// so a laid-out table row stays laid out with one in front.
fn quoted(mut spans: Vec<MdSpan>, depth: usize) -> Vec<MdSpan> {
    if depth == 0 {
        return spans;
    }
    let pre = spans.first().is_some_and(|span| span.pre);
    spans.insert(
        0,
        MdSpan {
            text: "│ ".repeat(depth),
            muted: true,
            pre,
            ..MdSpan::default()
        },
    );
    spans
}

/// Columns a quote's rail takes from the line, so what it wraps still fits.
fn rail_width(depth: usize) -> usize {
    depth * 2
}

/// Lay a table out in columns: each is as wide as its widest cell, capped at
/// the widest level that fits `width`, with cell text wrapping inside its own
/// column. A table that cannot hold readable columns is listed row by row,
/// which stays legible at any width.
fn table_lines(head: &[Vec<MdSpan>], rows: &[Vec<Vec<MdSpan>>], width: usize) -> Vec<Vec<MdSpan>> {
    let columns = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return Vec::new();
    }
    let cell_width = |cell: Option<&Vec<MdSpan>>| {
        cell.map_or(0, |runs| runs.iter().map(MdSpan::width).sum::<usize>())
    };
    let mut widths: Vec<usize> = (0..columns)
        .map(|column| {
            std::iter::once(head)
                .chain(rows.iter().map(Vec::as_slice))
                .map(|row| cell_width(row.get(column)))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    let gaps = COLUMN_GAP * (columns - 1);
    let budget = width.saturating_sub(gaps);
    if widths.iter().sum::<usize>() > budget {
        // bisected, since one cell can be arbitrarily wide
        let fits = |cap: usize| widths.iter().map(|w| (*w).min(cap)).sum::<usize>() <= budget;
        if !fits(MIN_COLUMN) {
            return list_lines(head, rows);
        }
        let (mut low, mut high) = (
            MIN_COLUMN,
            widths.iter().copied().max().unwrap_or(0).max(MIN_COLUMN),
        );
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if fits(mid) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        for w in &mut widths {
            *w = (*w).min(low);
        }
    }
    let mut out = Vec::new();
    if !head.is_empty() {
        out.extend(table_row(head, &widths));
        out.push(vec![MdSpan {
            text: widths
                .iter()
                .map(|w| "─".repeat(*w))
                .collect::<Vec<_>>()
                .join(&" ".repeat(COLUMN_GAP)),
            muted: true,
            pre: true,
            ..MdSpan::default()
        }]);
    }
    for row in rows {
        out.extend(table_row(row, &widths));
    }
    out
}

/// One table row: every cell wrapped to its column, then read across so a
/// cell that took three lines leaves its neighbours padded beside it.
fn table_row(row: &[Vec<MdSpan>], widths: &[usize]) -> Vec<Vec<MdSpan>> {
    let wrapped: Vec<Vec<Vec<MdSpan>>> = widths
        .iter()
        .enumerate()
        .map(|(column, w)| match row.get(column) {
            Some(cell) => wrap(cell, *w, *w),
            None => vec![Vec::new()],
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(0);
    (0..height)
        .map(|index| {
            let mut line: Vec<MdSpan> = Vec::new();
            for (column, cell) in wrapped.iter().enumerate() {
                let runs = cell.get(index).map_or(&[][..], Vec::as_slice);
                let used: usize = runs.iter().map(MdSpan::width).sum();
                if column > 0 {
                    line.push(MdSpan::plain(" ".repeat(COLUMN_GAP)));
                }
                line.extend(runs.iter().cloned());
                let pad = widths
                    .get(column)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(used);
                if pad > 0 && column + 1 < widths.len() {
                    line.push(MdSpan::plain(" ".repeat(pad)));
                }
            }
            for run in &mut line {
                run.pre = true;
            }
            line
        })
        .collect()
}

/// The narrow fallback: one `header: value` line per cell, a blank line
/// between rows.
fn list_lines(head: &[Vec<MdSpan>], rows: &[Vec<Vec<MdSpan>>]) -> Vec<Vec<MdSpan>> {
    let mut out = Vec::new();
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            let mut line = Vec::new();
            if let Some(label) = head.get(column) {
                line.extend(label.iter().cloned());
                line.push(MdSpan {
                    text: ": ".to_owned(),
                    muted: true,
                    ..MdSpan::default()
                });
            }
            line.extend(cell.iter().cloned());
            out.push(line);
        }
        out.push(Vec::new());
    }
    out.pop();
    out
}

/// Word-wrap the runs of one logical line to `first`/`rest` column budgets,
/// keeping every run's style. Breaks at spaces; a single token wider than the
/// budget is hard-split at character boundaries. Always yields at least one
/// (possibly empty) line.
pub fn wrap(runs: &[MdSpan], first: usize, rest: usize) -> Vec<Vec<MdSpan>> {
    if runs.first().is_some_and(|run| run.pre) {
        return vec![runs.to_vec()];
    }
    let words = split_words(runs);
    let mut out: Vec<Vec<MdSpan>> = Vec::new();
    let mut line: Vec<MdSpan> = Vec::new();
    let mut used = 0usize;
    let mut budget = first;
    let mut flush = |line: &mut Vec<MdSpan>, used: &mut usize, budget: &mut usize| {
        out.push(std::mem::take(line));
        *used = 0;
        *budget = rest;
    };
    for word in words {
        let wwidth: usize = word.iter().map(MdSpan::width).sum();
        let sep = usize::from(!line.is_empty());
        if used + sep + wwidth > budget && !line.is_empty() {
            flush(&mut line, &mut used, &mut budget);
        }
        if !line.is_empty() {
            line.push(MdSpan::plain(" "));
            used += 1;
        }
        if wwidth <= budget {
            line.extend(word);
            used += wwidth;
            continue;
        }
        let mut pending: Vec<(char, &MdSpan)> = Vec::new();
        let mut w = 0usize;
        let mut avail = budget
            .saturating_sub(used)
            .max(usize::from(line.is_empty()));
        for (c, span) in flatten(&word) {
            let cw = c.to_string().width();
            if w + cw > avail && !pending.is_empty() {
                line.extend(collect_spans(&pending));
                pending.clear();
                used += w;
                flush(&mut line, &mut used, &mut budget);
                w = 0;
                avail = budget;
            }
            pending.push((c, span));
            w += cw;
        }
        if !pending.is_empty() {
            line.extend(collect_spans(&pending));
            used += w;
        }
    }
    out.push(line);
    out.into_iter().map(coalesce).collect()
}

/// Merge adjacent same-style runs so a wrapped line is one run per style span,
/// not one per word.
fn coalesce(line: Vec<MdSpan>) -> Vec<MdSpan> {
    let mut merged: Vec<MdSpan> = Vec::new();
    for span in line {
        match merged.last_mut() {
            Some(last) if same_style(last, &span) => last.text.push_str(&span.text),
            _ => merged.push(span),
        }
    }
    merged
}

fn flatten(word: &[MdSpan]) -> Vec<(char, &MdSpan)> {
    word.iter()
        .flat_map(|span| span.text.chars().map(move |c| (c, span)))
        .collect()
}

/// Split runs into words (contiguous non-space styled pieces); every space,
/// including run-internal ones, is a break opportunity. Adjacent code runs join
/// into one word so a highlighted code line's whitespace (indentation, the gaps
/// between colored tokens) survives the wrap intact.
fn split_words(runs: &[MdSpan]) -> Vec<Vec<MdSpan>> {
    let mut words: Vec<Vec<MdSpan>> = Vec::new();
    let mut cur: Vec<MdSpan> = Vec::new();
    let mut cur_code = false;
    for run in runs {
        if run.code {
            if !cur.is_empty() && !cur_code {
                words.push(std::mem::take(&mut cur));
            }
            cur.push(run.clone());
            cur_code = true;
            continue;
        }
        if cur_code && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur_code = false;
        for (i, part) in run.text.split(' ').enumerate() {
            if i > 0 && !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            if !part.is_empty() {
                cur.push(MdSpan {
                    text: part.to_owned(),
                    ..run.clone()
                });
            }
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn collect_spans(chars: &[(char, &MdSpan)]) -> Vec<MdSpan> {
    let mut out: Vec<MdSpan> = Vec::new();
    for (c, span) in chars {
        match out.last_mut() {
            Some(last) if same_style(last, span) => last.text.push(*c),
            _ => out.push(MdSpan {
                text: c.to_string(),
                ..(*span).clone()
            }),
        }
    }
    out
}

fn same_style(a: &MdSpan, b: &MdSpan) -> bool {
    a.bold == b.bold
        && a.italic == b.italic
        && a.code == b.code
        && a.strike == b.strike
        && a.link == b.link
        && a.muted == b.muted
        && a.fg == b.fg
}

/// One code-block line as `code` runs, carrying each highlighted token's color
/// and leaving the gaps between them uncolored.
fn code_line_spans(text: &str, ranges: &[StyledRange]) -> Vec<MdSpan> {
    let mut sorted: Vec<&StyledRange> = ranges.iter().collect();
    sorted.sort_by_key(|r| r.range.start);
    let mut spans: Vec<MdSpan> = Vec::new();
    let mut pos = 0;
    for r in sorted {
        let start = r.range.start.max(pos).min(text.len());
        let end = r.range.end.min(text.len());
        if start >= end {
            continue;
        }
        if let Some(gap) = text.get(pos..start).filter(|g| !g.is_empty()) {
            spans.push(code_span(gap, None));
        }
        if let Some(seg) = text.get(start..end) {
            spans.push(code_span(seg, Some(r.fg)));
        }
        pos = end;
    }
    if let Some(rest) = text.get(pos..).filter(|g| !g.is_empty()) {
        spans.push(code_span(rest, None));
    }
    if spans.is_empty() {
        spans.push(code_span(text, None));
    }
    spans
}

fn code_span(text: &str, fg: Option<(u8, u8, u8)>) -> MdSpan {
    MdSpan {
        text: text.to_owned(),
        code: true,
        fg,
        ..MdSpan::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pane's usual comment column, wide enough that only a test about
    /// narrow layout has to say otherwise.
    const WIDTH: usize = 72;

    fn parse(src: &str) -> Vec<Vec<MdSpan>> {
        super::parse(src, None, WIDTH)
    }

    fn text(lines: &[Vec<MdSpan>]) -> String {
        lines
            .iter()
            .map(|line| line.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn inline_styles_tag_their_runs() {
        let lines = parse("use **bold** and `code` and *em* and ~~gone~~");
        let runs = &lines[0];
        let find = |t: &str| runs.iter().find(|s| s.text == t).cloned().unwrap();
        assert!(find("bold").bold);
        assert!(find("code").code);
        assert!(find("em").italic);
        assert!(find("gone").strike);
        assert!(!find("use ").bold);
    }

    #[test]
    fn soft_break_starts_a_new_line() {
        assert_eq!(text(&parse("first\nsecond")), "first\nsecond");
    }

    #[test]
    fn blank_line_separates_paragraphs() {
        assert_eq!(text(&parse("one\n\ntwo")), "one\ntwo");
    }

    #[test]
    fn code_block_lines_are_each_a_run() {
        let lines = parse("```\nlet x = 1;\nlet y = 2;\n```");
        assert_eq!(text(&lines), "let x = 1;\nlet y = 2;");
        assert!(lines.iter().all(|l| l.iter().all(|s| s.code)));
    }

    #[test]
    fn fenced_code_block_gets_syntax_colors() {
        let hl = Highlighter::default();
        let lines = super::parse("```rust\nfn f() {}\n```", Some(&hl), WIDTH);
        assert_eq!(text(&lines), "fn f() {}");
        assert!(lines[0].iter().all(|s| s.code), "still code runs");
        let colors: std::collections::HashSet<_> = lines[0].iter().filter_map(|s| s.fg).collect();
        assert!(
            colors.len() > 1,
            "keyword and identifier differ: {colors:?}"
        );
        // whitespace and layout survive the wrap
        assert_eq!(wrap(&lines[0], 40, 40).len(), 1);
        assert_eq!(
            wrap(&lines[0], 40, 40)[0]
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>(),
            "fn f() {}"
        );
    }

    #[test]
    fn code_block_without_highlighter_stays_plain() {
        let lines = super::parse("```rust\nfn f() {}\n```", None, WIDTH);
        assert!(lines[0].iter().all(|s| s.code && s.fg.is_none()));
    }

    #[test]
    fn bullets_get_a_muted_marker() {
        let lines = parse("- one\n- two");
        assert_eq!(text(&lines), "• one\n• two");
        assert!(lines[0][0].muted);
    }

    #[test]
    fn an_ordered_list_counts_its_items() {
        assert_eq!(
            text(&parse("1. one\n1. two\n1. three")),
            "1. one\n2. two\n3. three"
        );
        assert_eq!(text(&parse("7. seven\n8. eight")), "7. seven\n8. eight");
    }

    #[test]
    fn a_quote_carries_a_rail_on_every_line() {
        assert_eq!(text(&parse("> first\n> second")), "│ first\n│ second");
        assert!(parse("> quoted")[0][0].muted);
    }

    /// A quoted list is what an agent produces when it quotes the human back,
    /// and the lines nobody types by hand (items, code, tables, rules) reach
    /// the output by their own paths.
    #[test]
    fn a_quote_keeps_its_rail_around_every_kind_of_line() {
        assert_eq!(text(&parse("> - a\n> - b")), "│ • a\n│ • b");
        assert_eq!(text(&parse("> 1. a\n> 2. b")), "│ 1. a\n│ 2. b");
        assert_eq!(text(&parse("> - [ ] todo")), "│ • [ ] todo");
        assert_eq!(text(&parse("> ```\n> code\n> ```")), "│ code");
        assert_eq!(text(&parse("> | A |\n> |---|\n> | x |")), "│ A\n│ ─\n│ x");
        assert_eq!(text(&super::parse("> ---", None, 6)), "│ ────");
    }

    #[test]
    fn a_table_lays_out_in_columns_under_a_rule() {
        let lines =
            parse("| Lane | Ordering |\n|---|---|\n| Postgres | first |\n| Sinks | after |");
        assert_eq!(
            text(&lines),
            "Lane      Ordering\n────────  ────────\nPostgres  first\nSinks     after"
        );
        assert!(lines[0][0].bold, "the header reads as a header");
        assert!(lines[1][0].muted, "the rule is a rule");
    }

    #[test]
    fn a_table_cell_wraps_inside_its_own_column() {
        let lines = super::parse(
            "| Lane | What a failure costs |\n|---|---|\n| Postgres | a failed write means no sink hears anything |",
            None,
            34,
        );
        assert_eq!(
            text(&lines),
            [
                "Lane      What a failure costs",
                "────────  ────────────────────────",
                "Postgres  a failed write means no",
                "          sink hears anything",
            ]
            .join("\n"),
            "the neighbour stays padded beside the wrapped cell"
        );
    }

    /// The URL suffix belongs in the cell that carries the link, and a stray
    /// one lands as its own line under the table.
    #[test]
    fn a_link_inside_a_table_keeps_its_url_in_the_cell() {
        let lines =
            parse("| Doc | Link |\n|---|---|\n| spec | [rfc](https://x.dev/rfc) |\n\nafter");
        assert_eq!(
            text(&lines),
            [
                "Doc   Link",
                "────  ───────────────────────",
                "spec  rfc (https://x.dev/rfc)",
                "after",
            ]
            .join("\n")
        );
    }

    #[test]
    fn a_quote_opening_a_list_item_keeps_the_marker_on_its_line() {
        assert_eq!(text(&parse("- > quoted")), "│ • quoted");
        assert_eq!(text(&parse("1. > quoted")), "│ 1. quoted");
    }

    #[test]
    fn a_table_too_narrow_for_columns_is_listed_row_by_row() {
        let lines = super::parse(
            "| Lane | Ordering |\n|---|---|\n| Postgres | first |\n| Sinks | after |",
            None,
            16,
        );
        assert_eq!(
            text(&lines),
            "Lane: Postgres\nOrdering: first\n\nLane: Sinks\nOrdering: after"
        );
    }

    #[test]
    fn loose_list_keeps_the_bullet_with_its_text() {
        // blank lines between items make pulldown wrap each item in a paragraph
        assert_eq!(text(&parse("- one\n\n- two")), "• one\n• two");
    }

    #[test]
    fn task_list_markers_render() {
        let lines = parse("- [ ] todo\n- [x] done");
        assert_eq!(text(&lines), "• [ ] todo\n• [x] done");
    }

    #[test]
    fn code_block_indentation_survives_the_wrap() {
        let logical = parse("```\n    return 1\n```");
        let wrapped: Vec<Vec<MdSpan>> =
            logical.iter().flat_map(|line| wrap(line, 40, 40)).collect();
        assert_eq!(text(&wrapped), "    return 1");
    }

    #[test]
    fn raw_html_is_dropped() {
        // an agent reply that ends with a stray tag must not leak it
        assert_eq!(text(&parse("keep this</body>")), "keep this");
    }

    #[test]
    fn link_shows_text_then_muted_url() {
        let lines = parse("[docs](https://example.invalid)");
        let joined = text(&lines);
        assert!(joined.contains("docs"));
        assert!(joined.contains("https://example.invalid"));
        assert!(lines[0].iter().find(|s| s.text == "docs").unwrap().link);
    }

    #[test]
    fn wrap_breaks_at_spaces_and_keeps_style() {
        let runs = vec![
            MdSpan::plain("alpha "),
            MdSpan {
                text: "beta".to_owned(),
                bold: true,
                ..MdSpan::default()
            },
            MdSpan::plain(" gamma"),
        ];
        let wrapped = wrap(&runs, 10, 10);
        assert!(wrapped.len() >= 2);
        let bold = wrapped.iter().flatten().find(|s| s.text == "beta").unwrap();
        assert!(bold.bold, "style survives the wrap");
    }

    #[test]
    fn wrap_hard_splits_an_overlong_token() {
        let runs = vec![MdSpan::plain("abcdefghijklmnopqrstuvwxyz")];
        let wrapped = wrap(&runs, 8, 8);
        assert!(wrapped.len() > 1);
        for line in &wrapped {
            let width: usize = line.iter().map(MdSpan::width).sum();
            assert!(width <= 8, "{line:?}");
        }
        let joined: String = wrapped.iter().flatten().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "abcdefghijklmnopqrstuvwxyz");
    }
}
