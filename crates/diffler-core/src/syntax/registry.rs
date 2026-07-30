//! Language registry: maps a file path to its tree-sitter grammar, a configured
//! highlight configuration, and (where the grammar ships one) a tags query used
//! for scope/definition lookup. Built once and reused.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::{LazyLock, OnceLock};

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};
use tree_sitter_highlight::HighlightConfiguration;

use crate::syntax::MAX_PARSE_BYTES;

/// Capture names recognized during highlighting. A grammar capture like
/// `function.method` resolves to the longest matching prefix here (`function`),
/// so listing the general categories is enough to color every grammar.
pub const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "boolean",
    "comment",
    "conditional",
    "constant",
    "constant.builtin",
    "constructor",
    "field",
    "function",
    "function.builtin",
    "keyword",
    "label",
    "number",
    "operator",
    "parameter",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "spell",
    "storageclass",
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
    highlights: Cow<'static, str>,
    injections: Cow<'static, str>,
    tags_query: Option<Cow<'static, str>>,
    /// Compiling a query costs ~15ms, so a grammar pays only once someone opens
    /// a file in it. Both stay `None` when the grammar's query fails to
    /// compile: the file renders plain instead of erroring.
    config: OnceLock<Option<HighlightConfiguration>>,
    tags: OnceLock<Option<Query>>,
}

impl LangEntry {
    pub fn config(&self) -> Option<&HighlightConfiguration> {
        self.config
            .get_or_init(|| {
                HighlightConfiguration::new(
                    self.language.clone(),
                    self.name,
                    &self.highlights,
                    &self.injections,
                    "",
                )
                .ok()
                .map(|mut config| {
                    config.configure(HIGHLIGHT_NAMES);
                    config
                })
            })
            .as_ref()
    }

    pub fn tags(&self) -> Option<&Query> {
        self.tags
            .get_or_init(|| Query::new(&self.language, self.tags_query.as_deref()?).ok())
            .as_ref()
    }
}

/// The grammars, built once for the process. Registration only records the
/// grammar and its query text, so this costs microseconds; a theme switch
/// rebuilds the palette and reuses these.
pub static REGISTRY: LazyLock<LanguageRegistry> = LazyLock::new(LanguageRegistry::build);

pub struct LanguageRegistry {
    entries: Vec<LangEntry>,
    by_ext: HashMap<&'static str, usize>,
    by_name: HashMap<&'static str, usize>,
    by_filename: HashMap<&'static str, usize>,
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
            by_filename: HashMap::new(),
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
            format!(
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
            format!(
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
            format!(
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
            // the C++ query extends C's; alone it matches only C++ constructs
            format!(
                "{}\n{}",
                tree_sitter_c::HIGHLIGHT_QUERY,
                tree_sitter_cpp::HIGHLIGHT_QUERY
            ),
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
        r.register(
            "html",
            &["html", "htm"],
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
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
        // the grammar's own numeric patterns are guarded by Lua-style `%d`
        // predicates that tree-sitter's regex engine never matches, leaving
        // every number styled as a string; a later pattern wins, so this one
        // restores number coloring
        r.add(
            "sql",
            &["sql"],
            tree_sitter_sequel::LANGUAGE.into(),
            format!(
                "{}\n((literal) @number (#match? @number \"^[-+]?[0-9][0-9.]*$\"))\n",
                tree_sitter_sequel::HIGHLIGHTS_QUERY
            ),
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

        r.add(
            "toml",
            &["toml"],
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            None,
        );
        // the grammar crate ships a parser only; the query is vendored
        r.add(
            "hcl",
            &["tf", "tfvars", "hcl"],
            tree_sitter_hcl::LANGUAGE.into(),
            include_str!("../../queries/hcl/highlights.scm"),
            None,
        );
        r.add(
            "dockerfile",
            &["dockerfile", "containerfile"],
            arborium_dockerfile::language().into(),
            arborium_dockerfile::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "make",
            &["mk"],
            tree_sitter_make::LANGUAGE.into(),
            tree_sitter_make::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "lua",
            &["lua"],
            tree_sitter_lua::LANGUAGE.into(),
            tree_sitter_lua::HIGHLIGHTS_QUERY,
            Some(tree_sitter_lua::TAGS_QUERY),
        );
        r.add(
            "nix",
            &["nix"],
            tree_sitter_nix::LANGUAGE.into(),
            tree_sitter_nix::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "xml",
            &["xml", "xsd", "xslt", "svg"],
            tree_sitter_xml::LANGUAGE_XML.into(),
            tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            None,
        );
        r.add(
            "swift",
            &["swift"],
            tree_sitter_swift::LANGUAGE.into(),
            tree_sitter_swift::HIGHLIGHTS_QUERY,
            Some(tree_sitter_swift::TAGS_QUERY),
        );
        r.add(
            "scala",
            &["scala", "sbt"],
            tree_sitter_scala::LANGUAGE.into(),
            tree_sitter_scala::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "elixir",
            &["ex", "exs"],
            tree_sitter_elixir::LANGUAGE.into(),
            tree_sitter_elixir::HIGHLIGHTS_QUERY,
            Some(tree_sitter_elixir::TAGS_QUERY),
        );
        r.add(
            "zig",
            &["zig", "zon"],
            tree_sitter_zig::LANGUAGE.into(),
            tree_sitter_zig::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "haskell",
            &["hs"],
            tree_sitter_haskell::LANGUAGE.into(),
            tree_sitter_haskell::HIGHLIGHTS_QUERY,
            None,
        );
        r.add(
            "dart",
            &["dart"],
            tree_sitter_dart::LANGUAGE.into(),
            tree_sitter_dart::HIGHLIGHTS_QUERY,
            Some(tree_sitter_dart::TAGS_QUERY),
        );
        r.add(
            "powershell",
            &["ps1", "psm1", "psd1"],
            tree_sitter_powershell::LANGUAGE.into(),
            tree_sitter_powershell::HIGHLIGHTS_QUERY,
            None,
        );
        r.register(
            "svelte",
            &["svelte"],
            tree_sitter_svelte_ng::LANGUAGE.into(),
            // the svelte query extends html's, and its injections carry
            // `<script>` and `<style>` into the js and css grammars
            format!(
                "{}\n{}",
                tree_sitter_html::HIGHLIGHTS_QUERY,
                tree_sitter_svelte_ng::HIGHLIGHTS_QUERY
            ),
            tree_sitter_svelte_ng::INJECTIONS_QUERY,
            None,
        );
        r.name_files("make", &["makefile", "gnumakefile"]);
        r.name_files("dockerfile", &["dockerfile", "containerfile"]);

        r
    }

    /// Route files a build tool names outright (`Makefile`, `Dockerfile`).
    fn name_files(&mut self, name: &'static str, filenames: &'static [&'static str]) {
        let Some(&idx) = self.by_name.get(name) else {
            return;
        };
        for file in filenames {
            self.by_filename.insert(file, idx);
        }
    }

    fn add(
        &mut self,
        name: &'static str,
        extensions: &'static [&'static str],
        language: Language,
        highlights: impl Into<Cow<'static, str>>,
        tags: Option<&'static str>,
    ) {
        self.register(name, extensions, language, highlights, "", tags);
    }

    fn register(
        &mut self,
        name: &'static str,
        extensions: &'static [&'static str],
        language: Language,
        highlights: impl Into<Cow<'static, str>>,
        injections: impl Into<Cow<'static, str>>,
        tags: Option<&'static str>,
    ) {
        let idx = self.entries.len();
        self.entries.push(LangEntry {
            name,
            language,
            highlights: highlights.into(),
            injections: injections.into(),
            tags_query: tags.map(Cow::Borrowed),
            config: OnceLock::new(),
            tags: OnceLock::new(),
        });
        self.by_name.insert(name, idx);
        for ext in extensions {
            self.by_ext.insert(ext, idx);
        }
    }

    /// The entry whose grammar handles `path`, by extension or, for the files
    /// a build tool names outright (`Makefile`, `Dockerfile`), by basename.
    pub fn for_path(&self, path: &str) -> Option<&LangEntry> {
        let name = Path::new(path).file_name()?.to_str()?.to_ascii_lowercase();
        if let Some(&idx) = self.by_filename.get(name.as_str()) {
            return self.entries.get(idx);
        }
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
            "terraform" => "hcl",
            "docker" => "dockerfile",
            "makefile" => "make",
            "pwsh" | "ps" => "powershell",
            other => other,
        };
        let &idx = self.by_name.get(token).or_else(|| self.by_ext.get(token))?;
        self.entries.get(idx)
    }

    /// Highlight config for a tree-sitter injection language name (the inline
    /// markdown grammar, or a fenced code block's language). `None` leaves the
    /// injected region plain.
    pub fn config_for_injection(&self, lang: &str) -> Option<&HighlightConfiguration> {
        self.for_token(lang)?.config()
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
