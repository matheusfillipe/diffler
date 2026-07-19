//! Language registry: maps a file path to its tree-sitter grammar, a configured
//! highlight configuration, and (where the grammar ships one) a tags query used
//! for scope/definition lookup. Built once and reused.

use std::collections::HashMap;
use std::path::Path;

use tree_sitter::{Language, Query};
use tree_sitter_highlight::HighlightConfiguration;

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
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

pub struct LangEntry {
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
        let config = HighlightConfiguration::new(language.clone(), name, highlights, "", "")
            .ok()
            .map(|mut c| {
                c.configure(HIGHLIGHT_NAMES);
                c
            });
        let tags = tags.and_then(|q| Query::new(&language, q).ok());
        let idx = self.entries.len();
        self.entries.push(LangEntry {
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
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::build()
    }
}
