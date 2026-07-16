//! Minimal `CommonMark` rendering for comment and reply bodies. Parses to
//! theme-independent styled runs so the pure app layer stays free of ratatui
//! and the theme; [`crate::ui`] maps the flags to concrete styles at draw time.
//! Raw HTML is dropped rather than shown, so a stray tag in an agent reply does
//! not leak into the card.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

/// A styled text run with no line breaks. Flags compose (bold + italic), so
/// nested emphasis survives; `code`/`link`/`muted` additionally recolor.
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

/// Parse markdown into logical lines of styled runs (unwrapped). Line breaks,
/// block boundaries, list items, and code-block lines each start a new logical
/// line; a comment's own newlines are kept (GitHub renders them).
#[allow(clippy::too_many_lines)] // one arm per markdown event; a flat match reads best
pub fn parse(src: &str) -> Vec<Vec<MdSpan>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let mut lines: Vec<Vec<MdSpan>> = Vec::new();
    let mut line: Vec<MdSpan> = Vec::new();
    let mut flags = Flags::default();
    let mut list_depth: usize = 0;
    let mut code_block: Option<String> = None;
    let mut link_url: Option<String> = None;
    // a list item's own paragraph must not flush the bullet onto its own line
    let mut item_paragraph = false;

    let flush = |line: &mut Vec<MdSpan>, lines: &mut Vec<Vec<MdSpan>>| {
        if !line.is_empty() {
            lines.push(std::mem::take(line));
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
                flush(&mut line, &mut lines);
                flags.heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut line, &mut lines);
                flags.heading = false;
            }
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::Start(Tag::Item) => {
                flush(&mut line, &mut lines);
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                line.push(MdSpan {
                    text: format!("{indent}• "),
                    muted: true,
                    ..MdSpan::default()
                });
                item_paragraph = true;
            }
            Event::End(TagEnd::Item) => {
                item_paragraph = false;
                flush(&mut line, &mut lines);
            }
            Event::Start(Tag::Paragraph) => {
                if item_paragraph {
                    item_paragraph = false;
                } else {
                    flush(&mut line, &mut lines);
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut line, &mut lines);
                code_block = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(buf) = code_block.take() {
                    for text in buf.trim_end_matches('\n').split('\n') {
                        lines.push(vec![MdSpan {
                            text: text.to_owned(),
                            code: true,
                            ..MdSpan::default()
                        }]);
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
                    let shown: String = line.iter().map(|s| s.text.as_str()).collect();
                    if !url.is_empty() && !shown.contains(&url) {
                        line.push(MdSpan {
                            text: format!(" ({url})"),
                            muted: true,
                            ..MdSpan::default()
                        });
                    }
                }
            }
            Event::Text(text) => {
                if let Some(buf) = code_block.as_mut() {
                    buf.push_str(&text);
                } else {
                    line.push(MdSpan {
                        text: text.into_string(),
                        bold: flags.bold > 0 || flags.heading,
                        italic: flags.italic > 0,
                        strike: flags.strike > 0,
                        link: flags.link,
                        ..MdSpan::default()
                    });
                }
            }
            Event::Code(text) => line.push(MdSpan {
                text: text.into_string(),
                code: true,
                bold: flags.bold > 0 || flags.heading,
                italic: flags.italic > 0,
                link: flags.link,
                ..MdSpan::default()
            }),
            Event::TaskListMarker(checked) => line.push(MdSpan {
                text: if checked { "[x] " } else { "[ ] " }.to_owned(),
                muted: true,
                ..MdSpan::default()
            }),
            // a review comment's own line breaks are meaningful (GitHub renders
            // them), so a soft break starts a new line rather than a space
            Event::End(TagEnd::Paragraph) | Event::SoftBreak | Event::HardBreak => {
                flush(&mut line, &mut lines);
            }
            _ => {}
        }
    }
    flush(&mut line, &mut lines);
    lines
}

/// Word-wrap the runs of one logical line to `first`/`rest` column budgets,
/// keeping every run's style. Breaks at spaces; a single token wider than the
/// budget is hard-split at character boundaries. Always yields at least one
/// (possibly empty) line.
pub fn wrap(runs: &[MdSpan], first: usize, rest: usize) -> Vec<Vec<MdSpan>> {
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
/// including run-internal ones, is a break opportunity. Code runs stay whole so
/// their internal whitespace (indentation, alignment) survives the wrap.
fn split_words(runs: &[MdSpan]) -> Vec<Vec<MdSpan>> {
    let mut words: Vec<Vec<MdSpan>> = Vec::new();
    let mut cur: Vec<MdSpan> = Vec::new();
    for run in runs {
        if run.code {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            words.push(vec![run.clone()]);
            continue;
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn bullets_get_a_muted_marker() {
        let lines = parse("- one\n- two");
        assert_eq!(text(&lines), "• one\n• two");
        assert!(lines[0][0].muted);
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
