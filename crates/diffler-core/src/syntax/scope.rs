//! "What are we inside of": the chain of enclosing definitions (function,
//! class, method, …) for a line, derived from a grammar's tags query. The
//! result is plain data so the diff worker can compute it once and share it.

use std::collections::HashSet;

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator, Tree};

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

    /// 0-based start rows of every definition, sorted and deduped: the jump
    /// targets for function/definition motions.
    pub fn def_starts(&self) -> Vec<usize> {
        let mut rows: Vec<usize> = self.defs.iter().map(|d| d.start_row).collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    /// 0-based row span of the definition called `name`, start and end
    /// inclusive. What lets a reference open on a symbol and show its whole
    /// extent rather than seating a cursor on its first line.
    pub fn def_span(&self, name: &str) -> Option<(usize, usize)> {
        self.defs
            .iter()
            .find(|def| def.name == name)
            .map(|def| (def.start_row, def.end_row))
    }

    /// The innermost definition enclosing `line` (0-based): its name and its
    /// inclusive row span.
    pub fn enclosing(&self, line: usize) -> Option<(&str, usize, usize)> {
        self.defs
            .iter()
            .filter(|def| def.start_row <= line && line <= def.end_row)
            .min_by_key(|def| def.end_row - def.start_row)
            .map(|def| (def.name.as_str(), def.start_row, def.end_row))
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
    /// file is too large, or parsing fails, so callers show no breadcrumb.
    pub fn scope_index(&self, path: &str, content: &str) -> ScopeIndex {
        if content.len() > MAX_PARSE_BYTES {
            return ScopeIndex::default();
        }
        let Some(entry) = self.for_path(path) else {
            return ScopeIndex::default();
        };
        let Some(query) = entry.tags() else {
            return ScopeIndex::default();
        };
        let Some(tree) = parse(entry, content) else {
            return ScopeIndex::default();
        };
        tag_pass(query, &tree, content).0
    }
}

/// One run of a grammar's tags query over a parsed file: its definition spans,
/// and the start byte of every name the query captures, a definition's or a
/// reference's alike.
pub(crate) fn tag_pass(query: &Query, tree: &Tree, content: &str) -> (ScopeIndex, HashSet<usize>) {
    let names = query.capture_names();
    let bytes = content.as_bytes();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), bytes);
    let mut defs = Vec::new();
    let mut captured = HashSet::new();
    while let Some(m) = matches.next() {
        let mut span: Option<(usize, usize)> = None;
        let mut name: Option<String> = None;
        let mut qualified: Option<String> = None;
        for cap in m.captures {
            let cname = names.get(cap.index as usize).copied().unwrap_or("");
            if cname.starts_with("definition.") {
                let node = whole_definition(cap.node);
                span = Some((node.start_position().row, node.end_position().row));
            } else if cname == "name" {
                captured.insert(cap.node.start_byte());
                name = cap.node.utf8_text(bytes).ok().map(str::to_owned);
                qualified = cap
                    .node
                    .parent()
                    .filter(|parent| parent.kind() == "qualified_identifier")
                    .and_then(|parent| parent.utf8_text(bytes).ok())
                    .map(str::to_owned);
            }
        }
        if let (Some((start_row, end_row)), Some(name)) = (span, name) {
            for name in std::iter::once(name).chain(qualified) {
                defs.push(Def {
                    start_row,
                    end_row,
                    name,
                });
            }
        }
    }
    (ScopeIndex { defs }, captured)
}

/// The node a definition spans. C and C++ tag a function by its declarator,
/// which holds only the signature, so we climb to the function definition
/// around it to take in the body; a prototype has none and keeps its own.
fn whole_definition(node: Node<'_>) -> Node<'_> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "function_definition" => return parent,
            kind if kind.ends_with("declarator") => current = parent,
            _ => break,
        }
    }
    node
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

    #[test]
    fn every_tagged_language_spans_a_whole_function() {
        let reg = LanguageRegistry::build();
        let samples: &[(&str, &str, &str, (usize, usize))] = &[
            ("a.rs", "fn f() {\n    let x = 1;\n    x;\n}\n", "f", (0, 3)),
            ("a.py", "def f():\n    x = 1\n    return x\n", "f", (0, 2)),
            (
                "a.js",
                "function f() {\n  const x = 1;\n  return x;\n}\n",
                "f",
                (0, 3),
            ),
            (
                "a.ts",
                "function f(): number {\n  const x = 1;\n  return x;\n}\n",
                "f",
                (0, 3),
            ),
            (
                "a.tsx",
                "function f() {\n  const x = 1;\n  return x;\n}\n",
                "f",
                (0, 3),
            ),
            (
                "a.go",
                "package main\n\nfunc f() int {\n\tx := 1\n\treturn x\n}\n",
                "f",
                (2, 5),
            ),
            (
                "a.c",
                "static int f(int a,\n             int b) {\n    int x = a;\n    return x + b;\n}\n",
                "f",
                (0, 4),
            ),
            (
                "a.cpp",
                "static bool f(const int* a) {\n    int x = 1;\n    return x;\n}\n",
                "f",
                (0, 3),
            ),
            (
                "A.java",
                "class A {\n    int f() {\n        int x = 1;\n        return x;\n    }\n}\n",
                "f",
                (1, 4),
            ),
            (
                "a.cs",
                "class A {\n    int F() {\n        int x = 1;\n        return x;\n    }\n}\n",
                "F",
                (1, 4),
            ),
            ("a.rb", "def f\n  x = 1\n  x\nend\n", "f", (0, 3)),
            (
                "a.php",
                "<?php\nfunction f() {\n    $x = 1;\n    return $x;\n}\n",
                "f",
                (1, 4),
            ),
            (
                "a.lua",
                "function f()\n  local x = 1\n  return x\nend\n",
                "f",
                (0, 3),
            ),
            (
                "a.swift",
                "func f() -> Int {\n    let x = 1\n    return x\n}\n",
                "f",
                (0, 3),
            ),
            (
                "a.ex",
                "defmodule A do\n  def f do\n    x = 1\n    x\n  end\nend\n",
                "f",
                (1, 4),
            ),
            (
                "a.dart",
                "int f() {\n  var x = 1;\n  return x;\n}\n",
                "f",
                (0, 3),
            ),
        ];
        let wrong: Vec<String> = samples
            .iter()
            .filter_map(|(path, src, name, want)| {
                let got = reg.scope_index(path, src).def_span(name);
                (got != Some(*want)).then(|| format!("{path}: {got:?}, want {want:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn a_cpp_method_defined_outside_its_class_answers_to_its_qualified_name() {
        let reg = LanguageRegistry::build();
        let src = "void Cache::Link(int a) {\n    int x = a;\n    use(x);\n}\n";
        let index = reg.scope_index("a.cpp", src);
        assert_eq!(index.def_span("Cache::Link"), Some((0, 3)));
        assert_eq!(index.def_span("Link"), Some((0, 3)));
    }

    #[test]
    fn a_c_prototype_keeps_its_own_line() {
        let reg = LanguageRegistry::build();
        let src = "int f(int a);\n\nint g(void) {\n    return f(1);\n}\n";
        let index = reg.scope_index("a.c", src);
        assert_eq!(index.def_span("g"), Some((2, 4)));
        assert_eq!(index.def_span("f"), Some((0, 0)));
    }
}
