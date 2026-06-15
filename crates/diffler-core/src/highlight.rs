//! Whole-file syntax highlighting sliced into per-line styled ranges.
//! Highlighting whole files (not hunks) keeps stateful constructs like
//! multi-line strings correct across hunk boundaries.

use std::ops::Range;

use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
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

impl SyntaxTheme {
    fn embedded(self) -> EmbeddedThemeName {
        match self {
            Self::OneHalfDark => EmbeddedThemeName::OneHalfDark,
            Self::OneHalfLight => EmbeddedThemeName::OneHalfLight,
            Self::Dracula => EmbeddedThemeName::Dracula,
        }
    }
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
        let syntaxes = two_face::syntax::extra_newlines();
        let themes: EmbeddedLazyThemeSet = two_face::theme::extra();
        let theme = themes.get(syntax.embedded()).clone();
        Self { syntaxes, theme }
    }

    /// Highlight `content` as the language guessed from `path`'s extension.
    /// Returns one `Vec<StyledRange>` per line (without trailing newlines).
    /// Unknown languages produce empty ranges per line (plain rendering).
    pub fn highlight(&self, path: &str, content: &str) -> Vec<Vec<StyledRange>> {
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let Some(syntax) = self.syntaxes.find_syntax_by_extension(extension) else {
            return content.lines().map(|_| Vec::new()).collect();
        };
        let mut machine = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in syntect::util::LinesWithEndings::from(content) {
            let spans = machine
                .highlight_line(line, &self.syntaxes)
                .unwrap_or_default();
            let mut ranges = Vec::new();
            let mut pos = 0usize;
            let visible_len = line.trim_end_matches(['\n', '\r']).len();
            for (style, text) in spans {
                let start = pos;
                pos += text.len();
                let end = pos.min(visible_len);
                if start >= end {
                    continue;
                }
                ranges.push(StyledRange {
                    range: start..end,
                    fg: (style.foreground.r, style.foreground.g, style.foreground.b),
                    bold: style
                        .font_style
                        .contains(syntect::highlighting::FontStyle::BOLD),
                    italic: style
                        .font_style
                        .contains(syntect::highlighting::FontStyle::ITALIC),
                });
            }
            out.push(ranges);
        }
        out
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
        // line styled with more than one color: keyword vs identifier
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
        // the middle line is entirely inside the string: single styled run,
        // same color as the string-opening run on line 0
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
