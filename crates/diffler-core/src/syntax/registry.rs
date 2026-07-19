//! Language registry: maps a file path to its tree-sitter grammar, a configured
//! highlight configuration, and (where the grammar ships one) a tags query used
//! for scope/definition lookup. Built once and reused.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};
use tree_sitter_highlight::HighlightConfiguration;

use crate::syntax::MAX_PARSE_BYTES;

/// Capture names recognized during highlighting. A grammar capture like
/// `function.method` resolves to the longest matching prefix here (`function`),
/// so listing the general categories is enough to color every grammar.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "function",
    "function.builtin",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "string.escape",
    "string.special",
    "tag",
    "text.emphasis",
    "text.literal",
    "text.reference",
    "text.strong",
    "text.title",
    "text.uri",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

pub struct LangEntry {
    pub name: &'static str,
    pub language: Language,
    /// `None` when the grammar's highlight query failed to compile; the file
    /// then renders plain instead of erroring.
    pub config: Option<HighlightConfiguration>,
    /// Definition/tags query for scope lookup; `None` when the grammar ships no
    /// tags query.
    pub tags: Option<Query>,
}

pub struct LanguageRegistry {
    entries: Vec<LangEntry>,
    by_ext: HashMap<&'static str, usize>,
    by_name: HashMap<&'static str, usize>,
    /// The inline markdown highlight query, applied by hand over the block
    /// grammar's `(inline)` nodes: tree-sitter's generic injection does not
    /// drive the split markdown grammar's inline pass.
    markdown_inline_query: Option<Query>,
}

impl LanguageRegistry {
    /// Build the registry with every bundled grammar, reused for the session.
    // flat per-language registration table
    #[allow(clippy::too_many_lines)]
    pub fn build() -> Self {
        let mut r = Self {
            entries: Vec::new(),
            by_ext: HashMap::new(),
            by_name: HashMap::new(),
            markdown_inline_query: None,
        };

        r.add(
            "rust",
            &["rs"],
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            Some(tree_sitter_rust::TAGS_QUERY),
        );
        r.add(
            "python",
            &["py", "pyi"],
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY,
            Some(tree_sitter_python::TAGS_QUERY),
        );
        r.add(
            "javascript",
            &["js", "jsx", "mjs", "cjs"],
            tree_sitter_javascript::LANGUAGE.into(),
            &format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            ),
            Some(tree_sitter_javascript::TAGS_QUERY),
        );
        r.add(
            "typescript",
            &["ts", "mts", "cts"],
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            &format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
            Some(tree_sitter_typescript::TAGS_QUERY),
        );
        r.add(
            "tsx",
            &["tsx"],
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            &format!(
                "{}\n{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
            Some(tree_sitter_typescript::TAGS_QUERY),
        );
        r.add(
            "go",
            &["go"],
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::HIGHLIGHTS_QUERY,
            Some(tree_sitter_go::TAGS_QUERY),
        );
        r.add(
            "c",
            &["c", "h"],
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY,
            Some(tree_sitter_c::TAGS_QUERY),
        );
        r.add(
            "cpp",
            &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
            tree_sitter_cpp::LANGUAGE.into(),
            tree_sitter_cpp::HIGHLIGHT_QUERY,
            Some(tree_sitter_cpp::TAGS_QUERY),
        );
        r.add(
            "java",
            &["java"],
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_java::HIGHLIGHTS_QUERY,
            Some(tree_sitter_java::TAGS_QUERY),
        );
        r.add(
            "c-sharp",
            &["cs"],
            tree_sitter_c_sharp::LANGUAGE.into(),
            tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
            Some(tree_sitter_c_sharp::TAGS_QUERY),
        );
        r.add(
            "ruby",
            &["rb"],
            tree_sitter_ruby::LANGUAGE.into(),
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
            Some(tree_sitter_ruby::TAGS_QUERY),
        );
        r.add(
            "php",
            &["php"],
            tree_sitter_php::LANGUAGE_PHP.into(),
            tree_sitter_php::HIGHLIGHTS_QUERY,
            Some(tree_sitter_php::TAGS_QUERY),
        );
        r.add(
            "bash",
            &["sh", "bash", "zsh"],
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY,
            None,
        );
        r.add(
            "json",
            &["json"],
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "html",
            &["html", "htm"],
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "css",
            &["css"],
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "yaml",
            &["yml", "yaml"],
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            None,
        );
        // The block grammar highlights headings/markers and injects fenced code
        // into its own language; inline emphasis, code spans, and links come from
        // the by-hand inline pass below.
        r.register(
            "markdown",
            &["md", "markdown"],
            tree_sitter_md::LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            tree_sitter_md::INJECTION_QUERY_BLOCK,
            None,
        );
        let md_inline: Language = tree_sitter_md::INLINE_LANGUAGE.into();
        r.markdown_inline_query =
            Query::new(&md_inline, tree_sitter_md::HIGHLIGHT_QUERY_INLINE).ok();
        r.register(
            "markdown_inline",
            &[],
            md_inline,
            tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
            "",
            None,
        );

        r
    }

    fn add(
        &mut self,
        name: &'static str,
        extensions: &'static [&'static str],
        language: Language,
        highlights: &str,
        tags: Option<&str>,
    ) {
        self.register(name, extensions, language, highlights, "", tags);
    }

    fn register(
        &mut self,
        name: &'static str,
        extensions: &'static [&'static str],
        language: Language,
        highlights: &str,
        injections: &str,
        tags: Option<&str>,
    ) {
        let config =
            HighlightConfiguration::new(language.clone(), name, highlights, injections, "")
                .ok()
                .map(|mut c| {
                    c.configure(HIGHLIGHT_NAMES);
                    c
                });
        let tags = tags.and_then(|q| Query::new(&language, q).ok());
        let idx = self.entries.len();
        self.entries.push(LangEntry {
            name,
            language,
            config,
            tags,
        });
        self.by_name.insert(name, idx);
        for ext in extensions {
            self.by_ext.insert(ext, idx);
        }
    }

    /// The entry whose grammar handles `path`, keyed by file extension.
    pub fn for_path(&self, path: &str) -> Option<&LangEntry> {
        let ext = Path::new(path).extension()?.to_str()?;
        let &idx = self.by_ext.get(ext)?;
        self.entries.get(idx)
    }

    /// The entry for a markdown fence token (`rust`, `py`, `c++`, ...), matched
    /// by grammar name then extension.
    pub fn for_token(&self, token: &str) -> Option<&LangEntry> {
        let token = token.trim().to_ascii_lowercase();
        let token = match token.as_str() {
            "c++" => "cpp",
            "c#" | "csharp" => "cs",
            "shell" => "bash",
            "golang" => "go",
            other => other,
        };
        let &idx = self.by_name.get(token).or_else(|| self.by_ext.get(token))?;
        self.entries.get(idx)
    }

    /// Highlight config for a tree-sitter injection language name (the inline
    /// markdown grammar, or a fenced code block's language). `None` leaves the
    /// injected region plain.
    pub fn config_for_injection(&self, lang: &str) -> Option<&HighlightConfiguration> {
        self.for_token(lang)?.config.as_ref()
    }

    /// Inline markdown captures (emphasis, code spans, links) as byte range plus
    /// the recognized highlight name, narrowest span first so a first-match
    /// renderer picks the most specific. The inline grammar parses only the
    /// block grammar's `(inline)` node ranges, so block markers stay untouched.
    pub fn markdown_inline_spans(&self, content: &str) -> Vec<(Range<usize>, &'static str)> {
        if content.len() > MAX_PARSE_BYTES {
            return Vec::new();
        }
        let (Some(query), Some(block), Some(inline)) = (
            self.markdown_inline_query.as_ref(),
            self.by_name
                .get("markdown")
                .and_then(|&i| self.entries.get(i)),
            self.by_name
                .get("markdown_inline")
                .and_then(|&i| self.entries.get(i)),
        ) else {
            return Vec::new();
        };

        let mut bp = Parser::new();
        if bp.set_language(&block.language).is_err() {
            return Vec::new();
        }
        let Some(block_tree) = bp.parse(content, None) else {
            return Vec::new();
        };
        let ranges = inline_node_ranges(&block_tree);
        if ranges.is_empty() {
            return Vec::new();
        }

        let mut ip = Parser::new();
        if ip.set_included_ranges(&ranges).is_err() || ip.set_language(&inline.language).is_err() {
            return Vec::new();
        }
        let Some(inline_tree) = ip.parse(content, None) else {
            return Vec::new();
        };

        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut spans = Vec::new();
        let mut matches = cursor.matches(query, inline_tree.root_node(), content.as_bytes());
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let cname = names.get(cap.index as usize).copied().unwrap_or("");
                if let Some(name) = recognized_highlight(cname) {
                    spans.push((cap.node.byte_range(), name));
                }
            }
        }
        spans.sort_by_key(|(r, _)| r.end - r.start);
        spans
    }
}

/// Byte ranges of every `(inline)` node in a markdown block tree, in document
/// order. These are the regions the inline grammar reparses.
fn inline_node_ranges(tree: &Tree) -> Vec<tree_sitter::Range> {
    let mut ranges = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.kind() == "inline" && node.end_byte() > node.start_byte() {
            ranges.push(node.range());
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return ranges;
            }
        }
    }
}

/// The longest `HIGHLIGHT_NAMES` entry that is a dotted prefix of `capture`,
/// matching how tree-sitter resolves capture names to recognized highlights.
fn recognized_highlight(capture: &str) -> Option<&'static str> {
    HIGHLIGHT_NAMES
        .iter()
        .copied()
        .filter(|name| {
            capture == *name
                || capture
                    .strip_prefix(name)
                    .is_some_and(|r| r.starts_with('.'))
        })
        .max_by_key(|name| name.len())
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::build()
    }
}
