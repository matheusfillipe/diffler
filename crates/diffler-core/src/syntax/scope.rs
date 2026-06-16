//! "What are we inside of": the chain of enclosing definitions (function,
//! class, method, …) for a line, derived from a grammar's tags query. The
//! result is plain data so the diff worker can compute it once and share it.

use tree_sitter::{QueryCursor, StreamingIterator};

use crate::syntax::registry::LanguageRegistry;
use crate::syntax::{MAX_PARSE_BYTES, parse};

/// Definition spans for a file, queried per line for the enclosing-definition
/// breadcrumb.
#[derive(Debug, Clone, Default)]
pub struct ScopeIndex {
    defs: Vec<Def>,
}

#[derive(Debug, Clone)]
struct Def {
    start_row: usize,
    end_row: usize,
    name: String,
}

impl ScopeIndex {
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Names of the definitions enclosing `line` (0-based), outermost first. A
    /// line inside `class A` → `method` → body returns `["A", "method"]`.
    pub fn crumbs(&self, line: usize) -> Vec<String> {
        let mut hits: Vec<&Def> = self
            .defs
            .iter()
            .filter(|d| d.start_row <= line && line <= d.end_row)
            .collect();
        hits.sort_by(|a, b| {
            a.start_row
                .cmp(&b.start_row)
                .then(b.end_row.cmp(&a.end_row))
        });
        hits.into_iter().map(|d| d.name.clone()).collect()
    }
}

impl LanguageRegistry {
    /// Parse `content` and index its definition spans for scope lookup. Returns
    /// an empty index when the language is unsupported, has no tags query, the
    /// file is too large, or parsing fails — callers then show no breadcrumb.
    pub fn scope_index(&self, path: &str, content: &str) -> ScopeIndex {
        if content.len() > MAX_PARSE_BYTES {
            return ScopeIndex::default();
        }
        let Some(entry) = self.for_path(path) else {
            return ScopeIndex::default();
        };
        let Some(query) = entry.tags.as_ref() else {
            return ScopeIndex::default();
        };
        let Some(tree) = parse(entry, content) else {
            return ScopeIndex::default();
        };

        let names = query.capture_names();
        let bytes = content.as_bytes();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), bytes);
        let mut defs = Vec::new();
        while let Some(m) = matches.next() {
            let mut span: Option<(usize, usize)> = None;
            let mut name: Option<String> = None;
            for cap in m.captures {
                let cname = names.get(cap.index as usize).copied().unwrap_or("");
                if cname.starts_with("definition.") {
                    span = Some((cap.node.start_position().row, cap.node.end_position().row));
                } else if cname == "name" {
                    name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                }
            }
            if let (Some((start_row, end_row)), Some(name)) = (span, name) {
                defs.push(Def {
                    start_row,
                    end_row,
                    name,
                });
            }
        }
        ScopeIndex { defs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_python_scope_reads_class_then_method() {
        let reg = LanguageRegistry::build();
        let src = "class A:\n    def method(self):\n        x = 1\n        return x\n";
        let crumbs = reg.scope_index("a.py", src).crumbs(2);
        let names: Vec<&str> = crumbs.iter().map(String::as_str).collect();
        assert_eq!(names, ["A", "method"]);
    }

    #[test]
    fn rust_function_scope() {
        let reg = LanguageRegistry::build();
        let src = "fn outer() {\n    let y = 2;\n}\n";
        let crumbs = reg.scope_index("a.rs", src).crumbs(1);
        let names: Vec<&str> = crumbs.iter().map(String::as_str).collect();
        assert_eq!(names, ["outer"]);
    }

    #[test]
    fn scope_index_empty_for_unsupported_language() {
        let reg = LanguageRegistry::build();
        assert!(reg.scope_index("a.zzz-unknown", "whatever\n").is_empty());
    }

    #[test]
    fn line_outside_any_definition_has_no_crumbs() {
        let reg = LanguageRegistry::build();
        let src = "import os\n\ndef f():\n    pass\n";
        assert!(reg.scope_index("a.py", src).crumbs(0).is_empty());
    }
}
