//! The names a file uses, as its parse tree sees them: every identifier by
//! line and byte range, so a reader can follow one name through a change.

use std::ops::Range;

use crate::syntax::registry::LanguageRegistry;
use crate::syntax::scope::tag_pass;
use crate::syntax::{MAX_PARSE_BYTES, ScopeIndex, parse};

/// One identifier: its 0-based line, its byte range within that line, and the
/// name itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub line: usize,
    pub range: Range<usize>,
    pub name: String,
    /// The parse names a function, method or type here: a definition or a
    /// call the grammar's tags query marks, or a type position.
    pub item: bool,
}

impl LanguageRegistry {
    /// Every identifier in `content`, top to bottom, and its definition
    /// spans, from one parse. A file with no grammar, or too large to parse,
    /// falls back to plain words and no definitions; those words include the
    /// ones inside strings and comments.
    pub fn symbols(&self, path: &str, content: &str) -> (Vec<Ident>, ScopeIndex) {
        let entry = (content.len() <= MAX_PARSE_BYTES)
            .then(|| self.for_path(path))
            .flatten();
        let Some((entry, tree)) = entry.and_then(|entry| Some((entry, parse(entry, content)?)))
        else {
            return (words(content), ScopeIndex::default());
        };
        let (scope, tagged) = entry
            .tags()
            .map(|query| tag_pass(query, &tree, content))
            .unwrap_or_default();
        let mut idents = Vec::new();
        let mut cursor = tree.walk();
        loop {
            let node = cursor.node();
            if node.child_count() == 0 && !node.is_missing() && is_identifier(node.kind()) {
                let start = node.start_position();
                let len = node.end_byte() - node.start_byte();
                if let Some(name) = content
                    .get(node.start_byte()..node.end_byte())
                    .filter(|name| !name.is_empty())
                {
                    idents.push(Ident {
                        line: start.row,
                        range: start.column..start.column + len,
                        name: name.to_owned(),
                        item: node.kind().contains("type") || tagged.contains(&node.start_byte()),
                    });
                }
            }
            if cursor.goto_first_child() || cursor.goto_next_sibling() {
                continue;
            }
            loop {
                if !cursor.goto_parent() {
                    return (idents, scope);
                }
                if cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
}

/// Node kinds that name something. Grammars spell it many ways
/// (`field_identifier`, `type_identifier`, `simple_identifier`, PHP's
/// `name`, Ruby's `constant`), and only leaves are names.
fn is_identifier(kind: &str) -> bool {
    kind.ends_with("identifier") || kind == "name" || kind == "constant"
}

/// Every run of `[A-Za-z0-9_]` that does not start with a digit.
fn words(content: &str) -> Vec<Ident> {
    let mut out = Vec::new();
    for (line, text) in content.lines().enumerate() {
        let mut start = None;
        for (at, ch) in text
            .char_indices()
            .chain(std::iter::once((text.len(), ' ')))
        {
            let word_char = ch.is_ascii_alphanumeric() || ch == '_';
            match (start, word_char) {
                (None, true) => start = Some(at),
                (Some(from), false) => {
                    start = None;
                    let Some(token) = text.get(from..at) else {
                        continue;
                    };
                    if !token.starts_with(|first: char| first.is_ascii_digit()) {
                        out.push(Ident {
                            line,
                            range: from..at,
                            name: token.to_owned(),
                            item: false,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_on(idents: &[Ident], line: usize) -> Vec<&str> {
        idents
            .iter()
            .filter(|ident| ident.line == line)
            .map(|ident| ident.name.as_str())
            .collect()
    }

    #[test]
    fn a_parsed_file_yields_its_names_and_skips_strings_and_comments() {
        let reg = LanguageRegistry::build();
        let src = "fn total(price: u32, qty: u32) -> u32 {\n    // price times qty\n    let label = \"qty\";\n    price * qty\n}\n";
        let idents = reg.symbols("a.rs", src).0;
        assert_eq!(
            names_on(&idents, 0),
            ["total", "price", "qty"],
            "the grammar parses a primitive type as its own node"
        );
        assert!(names_on(&idents, 1).is_empty(), "a comment names nothing");
        assert_eq!(names_on(&idents, 2), ["label"], "a string names nothing");
        assert_eq!(names_on(&idents, 3), ["price", "qty"]);
        let price = idents.iter().find(|i| i.line == 3).expect("price");
        assert_eq!(
            &src.lines().nth(3).expect("line")[price.range.clone()],
            "price"
        );
    }

    #[test]
    fn a_call_or_a_definition_names_an_item_and_a_value_does_not() {
        let reg = LanguageRegistry::build();
        let src = "fn apply(lens: Lens) -> u32 {\n    lens.marks(total)\n}\n";
        let idents = reg.symbols("a.rs", src).0;
        let item = |line: usize, name: &str| {
            idents
                .iter()
                .find(|ident| ident.line == line && ident.name == name)
                .is_some_and(|ident| ident.item)
        };
        assert!(item(0, "apply"), "a definition");
        assert!(item(0, "Lens"), "a type position");
        assert!(item(1, "marks"), "a method call");
        assert!(!item(1, "lens"), "the value it is called on");
        assert!(!item(1, "total"), "an argument");
    }

    #[test]
    fn a_file_without_a_grammar_falls_back_to_words() {
        let reg = LanguageRegistry::build();
        let idents = reg.symbols("notes.zzz", "total = price * 2qty\n").0;
        assert_eq!(names_on(&idents, 0), ["total", "price"]);
    }
}
