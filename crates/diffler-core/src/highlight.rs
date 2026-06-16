//! Whole-file syntax highlighting via tree-sitter, sliced into per-line styled
//! ranges. Highlighting whole files (not hunks) keeps multi-line constructs
//! like strings correct across hunk boundaries. Unknown languages and parse
//! failures degrade to plain (empty) ranges so rendering never breaks.

use std::ops::Range;

use tree_sitter_highlight::{HighlightEvent, Highlighter as TsHighlighter};

use crate::syntax::{HIGHLIGHT_NAMES, LanguageRegistry};

pub struct Highlighter {
    registry: LanguageRegistry,
    theme: SyntaxTheme,
}

/// Syntax-highlight palette, paired with a UI theme so foreground colors stay
/// legible against the diff backgrounds (a dark UI needs dark-theme syntax, a
/// light UI light-theme syntax).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyntaxTheme {
    #[default]
    OneHalfDark,
    OneHalfLight,
    Dracula,
}

/// Foreground color + style for a byte range of one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledRange {
    pub range: Range<usize>,
    pub fg: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new(SyntaxTheme::default())
    }
}

impl Highlighter {
    /// Build a highlighter whose foregrounds come from `syntax`.
    pub fn new(syntax: SyntaxTheme) -> Self {
        Self {
            registry: LanguageRegistry::build(),
            theme: syntax,
        }
    }

    /// Highlight `content` as the language guessed from `path`'s extension.
    /// Returns one `Vec<StyledRange>` per line (without trailing newlines).
    /// Unknown languages produce empty ranges per line (plain rendering).
    pub fn highlight(&self, path: &str, content: &str) -> Vec<Vec<StyledRange>> {
        let bounds = crate::syntax::line_bounds(content);
        let mut out: Vec<Vec<StyledRange>> = vec![Vec::new(); bounds.len()];

        let Some(entry) = self.registry.for_path(path) else {
            return out;
        };
        let Some(config) = entry.config.as_ref() else {
            return out;
        };

        let mut ts = TsHighlighter::new();
        let Ok(events) = ts.highlight(config, content.as_bytes(), None, |_| None) else {
            return out;
        };

        let starts: Vec<usize> = bounds.iter().map(|&(s, _)| s).collect();
        let mut stack: Vec<usize> = Vec::new();
        for event in events {
            let Ok(event) = event else {
                return out;
            };
            match event {
                HighlightEvent::HighlightStart(h) => stack.push(h.0),
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    if let Some(&name_idx) = stack.last()
                        && let Some(name) = HIGHLIGHT_NAMES.get(name_idx)
                        && let Some(style) = self.theme.style(name)
                    {
                        crate::syntax::split_range_by_line(
                            &bounds,
                            &starts,
                            &(start..end),
                            |li, r| {
                                if let Some(line) = out.get_mut(li) {
                                    line.push(StyledRange {
                                        range: r,
                                        fg: style.fg,
                                        bold: style.bold,
                                        italic: style.italic,
                                    });
                                }
                            },
                        );
                    }
                }
            }
        }
        out
    }

    /// Definition breadcrumb index for `content`, computed via the same grammar
    /// registry used for highlighting. Empty for unsupported languages.
    pub fn scope_index(&self, path: &str, content: &str) -> crate::syntax::ScopeIndex {
        self.registry.scope_index(path, content)
    }

    /// Set AST-diff char-precise emphasis on `file`. Returns `false` (caller
    /// should fall back to the textual engine) when unavailable.
    pub fn syntactic_emphasis(&self, file: &mut crate::model::FileDiff) -> bool {
        self.registry.syntactic_emphasis(file)
    }
}

struct StyleSpec {
    fg: (u8, u8, u8),
    bold: bool,
    italic: bool,
}

impl SyntaxTheme {
    /// Style for a tree-sitter capture name, matched by its leading category
    /// (`function.method` -> `function`). `None` leaves the span at default fg.
    fn style(self, name: &str) -> Option<StyleSpec> {
        let category = name.split('.').next().unwrap_or(name);
        let italic = category == "comment";
        let fg = self.color(category)?;
        Some(StyleSpec {
            fg,
            bold: false,
            italic,
        })
    }

    fn color(self, category: &str) -> Option<(u8, u8, u8)> {
        let c = match self {
            SyntaxTheme::OneHalfDark => match category {
                "keyword" | "label" => (198, 120, 221),
                "function" => (97, 175, 239),
                "type" | "constructor" => (229, 192, 123),
                "string" => (152, 195, 121),
                "comment" => (92, 99, 112),
                "constant" | "number" | "attribute" => (209, 154, 102),
                "operator" | "escape" => (86, 182, 194),
                "property" | "tag" => (224, 108, 117),
                "variable" | "punctuation" => (171, 178, 191),
                _ => return None,
            },
            SyntaxTheme::OneHalfLight => match category {
                "keyword" | "label" => (166, 38, 164),
                "function" => (64, 120, 242),
                "type" | "constructor" => (193, 132, 1),
                "string" => (80, 161, 79),
                "comment" => (160, 161, 167),
                "constant" | "number" | "attribute" => (152, 104, 1),
                "operator" | "escape" => (1, 132, 188),
                "property" | "tag" => (228, 86, 73),
                "variable" | "punctuation" => (56, 58, 66),
                _ => return None,
            },
            SyntaxTheme::Dracula => match category {
                "keyword" | "label" | "tag" | "operator" => (255, 121, 198),
                "function" | "property" => (80, 250, 123),
                "type" | "constructor" => (139, 233, 253),
                "string" => (241, 250, 140),
                "comment" => (98, 114, 164),
                "constant" | "number" => (189, 147, 249),
                "escape" | "attribute" => (255, 184, 108),
                "variable" | "punctuation" => (248, 248, 242),
                _ => return None,
            },
        };
        Some(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_keywords_get_distinct_color() {
        let hl = Highlighter::default();
        let lines = hl.highlight("a.py", "def f():\n    return 1\n");
        assert_eq!(lines.len(), 2);
        let colors: std::collections::HashSet<(u8, u8, u8)> =
            lines[0].iter().map(|r| r.fg).collect();
        assert!(colors.len() > 1, "expected multiple colors, got {colors:?}");
    }

    #[test]
    fn ranges_cover_within_line_bounds() {
        let hl = Highlighter::default();
        let src = "fn main() { let x = \"hi\"; }\n";
        let lines = hl.highlight("a.rs", src);
        let visible = src.trim_end();
        for r in &lines[0] {
            assert!(r.range.end <= visible.len());
            assert!(r.range.start < r.range.end);
        }
    }

    #[test]
    fn multiline_string_state_carries_across_lines() {
        let hl = Highlighter::default();
        let src = "s = \"\"\"first\nsecond\nthird\"\"\"\nx = 1\n";
        let lines = hl.highlight("a.py", src);
        let string_color = lines[0].iter().last().map(|r| r.fg).expect("line 0 styled");
        assert!(
            lines[1].iter().all(|r| r.fg == string_color),
            "inside-string line must keep string color"
        );
    }

    #[test]
    fn unknown_extension_yields_plain_lines() {
        let hl = Highlighter::default();
        let lines = hl.highlight("file.zzz-unknown", "a\nb\n");
        assert_eq!(lines, vec![Vec::new(), Vec::new()]);
    }

    #[test]
    fn syntax_theme_changes_the_foreground_palette() {
        let src = "fn main() { let x = 1; }\n";
        let dark = Highlighter::new(SyntaxTheme::OneHalfDark).highlight("a.rs", src);
        let light = Highlighter::new(SyntaxTheme::OneHalfLight).highlight("a.rs", src);
        assert_ne!(dark, light, "a different syntax theme recolors the line");
    }
}
