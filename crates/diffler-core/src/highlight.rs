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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SyntaxTheme {
    #[default]
    OneHalfDark,
    OneHalfLight,
    Dracula,
    CatppuccinMocha,
    TokyoNight,
    GruvboxDark,
    Nord,
    RosePine,
    Kanagawa,
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
        self.highlight_entry(self.registry.for_path(path), content)
    }

    /// Highlight `content` as a markdown fence token (`rust`, `py`, ...).
    pub fn highlight_lang(&self, token: &str, content: &str) -> Vec<Vec<StyledRange>> {
        self.highlight_entry(self.registry.for_token(token), content)
    }

    fn highlight_entry(
        &self,
        entry: Option<&crate::syntax::registry::LangEntry>,
        content: &str,
    ) -> Vec<Vec<StyledRange>> {
        let bounds = crate::syntax::line_bounds(content);
        let mut out: Vec<Vec<StyledRange>> = vec![Vec::new(); bounds.len()];

        if content.len() > crate::syntax::MAX_PARSE_BYTES {
            return out;
        }
        let Some(entry) = entry else {
            return out;
        };
        let Some(config) = entry.config.as_ref() else {
            return out;
        };

        let mut ts = TsHighlighter::new();
        let registry = &self.registry;
        let Ok(events) = ts.highlight(config, content.as_bytes(), None, move |lang| {
            registry.config_for_injection(lang)
        }) else {
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
                        push_styled(&mut out, &bounds, &starts, &(start..end), &style);
                    }
                }
            }
        }

        if entry.name == "markdown" {
            for (range, name) in self.registry.markdown_inline_spans(content) {
                if let Some(style) = self.theme.style(name) {
                    push_styled(&mut out, &bounds, &starts, &range, &style);
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

fn push_styled(
    out: &mut [Vec<StyledRange>],
    bounds: &[(usize, usize)],
    starts: &[usize],
    range: &Range<usize>,
    style: &StyleSpec,
) {
    crate::syntax::split_range_by_line(bounds, starts, range, |li, r| {
        if let Some(line) = out.get_mut(li) {
            line.push(StyledRange {
                range: r,
                fg: style.fg,
                bold: style.bold,
                italic: style.italic,
            });
        }
    });
}

/// Palette category and face for markdown `text.*` captures, reusing the general
/// syntax colors (headings as functions, code spans as strings, links as
/// properties) so every theme styles markdown with no extra color tables.
fn markdown_face(name: &str) -> Option<(&'static str, bool, bool)> {
    let face = match name {
        "text.title" => ("function", true, false),
        "text.strong" => ("variable", true, false),
        "text.emphasis" => ("variable", false, true),
        "text.literal" => ("string", false, false),
        "text.uri" | "text.reference" => ("property", false, false),
        _ => return None,
    };
    Some(face)
}

impl SyntaxTheme {
    /// Style for a tree-sitter capture name, matched by its leading category
    /// (`function.method` -> `function`). `None` leaves the span at default fg.
    fn style(self, name: &str) -> Option<StyleSpec> {
        if let Some((category, bold, italic)) = markdown_face(name) {
            return Some(StyleSpec {
                fg: self.color(category)?,
                bold,
                italic,
            });
        }
        let category = name.split('.').next().unwrap_or(name);
        let italic = category == "comment";
        let fg = self.color(category)?;
        Some(StyleSpec {
            fg,
            bold: false,
            italic,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn color(self, category: &str) -> Option<(u8, u8, u8)> {
        let c = match self {
            SyntaxTheme::OneHalfDark => match category {
                "keyword" | "label" => (198, 120, 221),
                "function" => (97, 175, 239),
                "type" | "constructor" => (229, 192, 123),
                "string" => (152, 195, 121),
                // brighter than One Dark's default so comments stay legible on
                // the added/removed diff backgrounds, not just the editor bg
                "comment" => (126, 134, 145),
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
            SyntaxTheme::CatppuccinMocha => match category {
                "keyword" | "label" => (203, 166, 247),
                "function" => (137, 180, 250),
                "type" | "constructor" => (249, 226, 175),
                "string" => (166, 227, 161),
                "comment" => (127, 132, 156),
                "constant" | "number" | "attribute" => (250, 179, 135),
                "operator" | "escape" => (137, 220, 235),
                "property" | "tag" => (243, 139, 168),
                "variable" | "punctuation" => (205, 214, 244),
                _ => return None,
            },
            SyntaxTheme::TokyoNight => match category {
                "keyword" | "label" => (187, 154, 247),
                "function" => (122, 162, 247),
                "type" | "constructor" => (42, 195, 222),
                "string" => (158, 206, 106),
                "comment" => (99, 109, 150),
                "constant" | "number" | "attribute" => (255, 158, 100),
                "operator" | "escape" => (137, 221, 255),
                "property" | "tag" => (247, 118, 142),
                "variable" | "punctuation" => (192, 202, 245),
                _ => return None,
            },
            SyntaxTheme::GruvboxDark => match category {
                "keyword" | "label" => (251, 73, 52),
                "function" => (184, 187, 38),
                "type" | "constructor" => (250, 189, 47),
                "string" => (142, 192, 124),
                "comment" => (146, 131, 116),
                "constant" | "number" => (211, 134, 155),
                "operator" | "escape" | "attribute" => (254, 128, 25),
                "property" | "tag" => (131, 165, 152),
                "variable" | "punctuation" => (235, 219, 178),
                _ => return None,
            },
            SyntaxTheme::Nord => match category {
                "keyword" | "label" | "operator" | "escape" => (129, 161, 193),
                "function" => (136, 192, 208),
                "type" | "constructor" | "property" | "tag" => (143, 188, 187),
                "string" => (163, 190, 140),
                "comment" => (123, 136, 161),
                "constant" | "number" | "attribute" => (180, 142, 173),
                "variable" | "punctuation" => (216, 222, 233),
                _ => return None,
            },
            SyntaxTheme::RosePine => match category {
                "keyword" | "label" | "operator" | "escape" => (49, 116, 143),
                "function" => (235, 188, 186),
                "type" | "constructor" | "property" | "tag" => (156, 207, 216),
                "string" => (246, 193, 119),
                "comment" => (129, 124, 153),
                "constant" | "number" | "attribute" => (196, 167, 231),
                "variable" | "punctuation" => (224, 222, 244),
                _ => return None,
            },
            SyntaxTheme::Kanagawa => match category {
                "keyword" | "label" => (149, 127, 184),
                "function" => (126, 156, 216),
                "type" | "constructor" => (122, 168, 159),
                "string" => (152, 187, 108),
                "comment" => (144, 140, 128),
                "constant" | "number" | "attribute" => (210, 126, 153),
                "operator" | "escape" => (127, 180, 202),
                "property" | "tag" => (106, 149, 137),
                "variable" | "punctuation" => (220, 215, 186),
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
    fn yaml_is_highlighted() {
        let hl = Highlighter::default();
        let lines = hl.highlight("ci.yml", "name: CI\non: push\njobs:\n  lint: {}\n");
        assert!(
            lines.iter().any(|line| !line.is_empty()),
            "expected styled ranges for a .yml file"
        );
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
    fn markdown_highlights_headings_and_inline_code() {
        let hl = Highlighter::default();
        let src = "# Title\n\nSome `code` and **bold** text.\n";
        let lines = hl.highlight("readme.md", src);
        assert!(!lines[0].is_empty(), "heading line should be styled");
        // `code` is styled by the by-hand inline pass over the block (inline) node
        assert!(
            lines[2].iter().any(|r| r.fg == (152, 195, 121)),
            "inline `code` should get the string color"
        );
    }

    #[test]
    fn markdown_inline_code_offset_is_absolute_not_range_relative() {
        // inline content starts well past byte 0 (after a heading and blank
        // lines); the code span must still land on its own line
        let hl = Highlighter::default();
        let src = "# A longer heading here\n\nintro line\n\nthen `code` appears.\n";
        let lines = hl.highlight("readme.md", src);
        let code_line = "then `code` appears.";
        let styled: Vec<_> = lines[4]
            .iter()
            .filter(|r| r.fg == (152, 195, 121))
            .collect();
        assert!(!styled.is_empty(), "code span should be styled on line 4");
        for r in styled {
            assert!(
                r.range.end <= code_line.len(),
                "range {:?} escapes the line (offsets not absolute)",
                r.range
            );
            assert_eq!(&code_line[r.range.clone()], "`code`");
        }
    }

    #[test]
    fn markdown_fenced_code_block_gets_language_highlight() {
        let hl = Highlighter::default();
        let src = "text\n\n```rust\nfn f() {}\n```\n";
        let lines = hl.highlight("readme.md", src);
        // `fn` keyword inside the fence is highlighted by the injected rust grammar
        assert!(
            lines[3].iter().any(|r| r.fg == (198, 120, 221)),
            "fenced rust `fn` should get the keyword color"
        );
    }

    #[test]
    fn markdown_fence_tag_resolves_by_extension() {
        // an `rs` fence tag is a file extension, not a grammar name; it resolves
        // to rust through the extension table
        let hl = Highlighter::default();
        let src = "text\n\n```rs\nfn f() {}\n```\n";
        let lines = hl.highlight("readme.md", src);
        assert!(
            lines[3].iter().any(|r| r.fg == (198, 120, 221)),
            "an `rs` fence should resolve to rust via by_ext"
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
