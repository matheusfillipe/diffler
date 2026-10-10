//! The symbol lens: the names on one diff line and their uses on lines the
//! diff shows. A local reaches its enclosing function; a function, method or
//! type reaches every file of the diff.

use std::collections::{HashMap, HashSet};
use std::ops::{Range, RangeInclusive};

use crate::highlight::Highlighter;
use crate::model::{DiffModel, LineKind};
use crate::syntax::{Ident, ScopeIndex};

/// One per digit key.
pub const MAX_SYMBOLS: usize = 9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensOrigin {
    pub path: String,
    pub on_old_side: bool,
    pub line: u32,
}

/// Both sides' text and the lines the diff shows on each. A context line
/// counts on the new side only, so it counts once.
#[derive(Debug, Clone)]
pub struct LensFile {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub old_lines: HashSet<u32>,
    pub new_lines: HashSet<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// A local of the named function.
    Function(String),
    /// A top-level name of its file.
    File,
    /// A definition, call or type position.
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensSymbol {
    pub name: String,
    pub reach: Reach,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensUse {
    pub symbol: usize,
    pub path: String,
    pub on_old_side: bool,
    pub line: u32,
    pub range: Range<usize>,
}

struct Side {
    idents: Vec<Ident>,
    scope: ScopeIndex,
}

impl Side {
    fn read(highlighter: &Highlighter, path: &str, text: Option<&str>) -> Option<Self> {
        let text = text?;
        let (idents, scope) = highlighter.symbols(path, text);
        Some(Self { idents, scope })
    }
}

/// Parses both sides of every file, so we run it on a worker.
pub fn compute(highlighter: &Highlighter, origin: &LensOrigin, files: &[LensFile]) -> LensData {
    let sides: Vec<(Option<Side>, Option<Side>)> = files
        .iter()
        .map(|file| {
            (
                Side::read(highlighter, &file.path, file.old_text.as_deref()),
                Side::read(highlighter, &file.path, file.new_text.as_deref()),
            )
        })
        .collect();
    let origin_side = files
        .iter()
        .position(|file| file.path == origin.path)
        .and_then(|at| sides.get(at))
        .and_then(|(old, new)| {
            if origin.on_old_side {
                old.as_ref()
            } else {
                new.as_ref()
            }
        });
    let symbols = line_symbols(origin, origin_side);
    let origin_span = origin_side
        .and_then(|side| side.scope.enclosing(origin.line.saturating_sub(1) as usize))
        .map(|(_, start, end)| start..=end);
    let uses = symbol_uses(origin, origin_span.as_ref(), files, &sides, &symbols);
    LensData {
        origin: origin.clone(),
        symbols,
        uses,
    }
}

fn line_symbols(origin: &LensOrigin, origin_side: Option<&Side>) -> Vec<LensSymbol> {
    let origin_row = origin.line.saturating_sub(1) as usize;
    let mut names: Vec<String> = Vec::new();
    let mut items: HashSet<&str> = HashSet::new();
    for ident in origin_side.map_or(&[][..], |side| side.idents.as_slice()) {
        if ident.line != origin_row {
            continue;
        }
        if ident.item {
            items.insert(ident.name.as_str());
        }
        if !names.contains(&ident.name) && names.len() < MAX_SYMBOLS {
            names.push(ident.name.clone());
        }
    }
    let enclosing = origin_side.and_then(|side| side.scope.enclosing(origin_row));
    names
        .into_iter()
        .map(|name| {
            // a local sharing a name with a function elsewhere stays local
            let reach = if items.contains(name.as_str()) {
                Reach::Diff
            } else if let Some((function, _, _)) = enclosing {
                Reach::Function(function.to_owned())
            } else {
                Reach::File
            };
            LensSymbol { name, reach }
        })
        .collect()
}

fn symbol_uses(
    origin: &LensOrigin,
    origin_span: Option<&RangeInclusive<usize>>,
    files: &[LensFile],
    sides: &[(Option<Side>, Option<Side>)],
    symbols: &[LensSymbol],
) -> Vec<LensUse> {
    let index: HashMap<&str, usize> = symbols
        .iter()
        .enumerate()
        .map(|(at, symbol)| (symbol.name.as_str(), at))
        .collect();
    let mut uses = Vec::new();
    for (file, (old, new)) in files.iter().zip(sides) {
        for (on_old_side, side, shown) in
            [(true, old, &file.old_lines), (false, new, &file.new_lines)]
        {
            let Some(side) = side else {
                continue;
            };
            for ident in &side.idents {
                let line = u32::try_from(ident.line + 1).unwrap_or(u32::MAX);
                let Some(&symbol) = index.get(ident.name.as_str()) else {
                    continue;
                };
                if !shown.contains(&line) {
                    continue;
                }
                let reaches = match symbols.get(symbol).map(|s| &s.reach) {
                    Some(Reach::Diff) => true,
                    Some(Reach::File) => file.path == origin.path,
                    Some(Reach::Function(function)) if file.path == origin.path => {
                        // a file can define two functions of one name, so on the
                        // origin's own side we hold to the one the line sits in
                        let span = if on_old_side == origin.on_old_side {
                            origin_span.cloned()
                        } else {
                            side.scope
                                .def_span(function)
                                .map(|(start, end)| start..=end)
                        };
                        span.is_some_and(|span| span.contains(&ident.line))
                    }
                    Some(Reach::Function(_)) | None => false,
                };
                if reaches {
                    uses.push(LensUse {
                        symbol,
                        path: file.path.clone(),
                        on_old_side,
                        line,
                        range: ident.range.clone(),
                    });
                }
            }
        }
    }
    uses
}

pub fn lens_files(model: &DiffModel) -> Vec<LensFile> {
    model
        .files
        .iter()
        .filter(|file| !file.binary)
        .map(|file| {
            let mut old_lines = HashSet::new();
            let mut new_lines = HashSet::new();
            for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
                match (line.kind, line.old_no, line.new_no) {
                    (LineKind::Deleted, Some(old), _) => {
                        old_lines.insert(old);
                    }
                    (LineKind::Added | LineKind::Context, _, Some(new)) => {
                        new_lines.insert(new);
                    }
                    _ => {}
                }
            }
            LensFile {
                path: file.path.clone(),
                old_text: file.old_text.clone(),
                new_text: file.new_text.clone(),
                old_lines,
                new_lines,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct LensData {
    pub origin: LensOrigin,
    pub symbols: Vec<LensSymbol>,
    pub uses: Vec<LensUse>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines the lens finds `a` on from line 6 of two functions that
    /// each bind their own `a`.
    fn uses_of_a(highlighter: &Highlighter, path: &str) -> Vec<u32> {
        let text = "fn f() {\n    let a = 1;\n}\n\nfn f() {\n    let a = 2;\n    a\n}\n";
        let file = LensFile {
            path: path.to_owned(),
            old_text: None,
            new_text: Some(text.to_owned()),
            old_lines: HashSet::new(),
            new_lines: (1..=8).collect(),
        };
        let origin = LensOrigin {
            path: path.to_owned(),
            on_old_side: false,
            line: 6,
        };
        let lens = compute(highlighter, &origin, &[file]);
        let a = lens
            .symbols
            .iter()
            .position(|symbol| symbol.name == "a")
            .expect("a is named on the line");
        lens.uses
            .iter()
            .filter(|found| found.symbol == a)
            .map(|found| found.line)
            .collect()
    }

    #[test]
    fn a_local_stays_in_the_function_its_line_sits_in() {
        assert_eq!(uses_of_a(&Highlighter::default(), "a.rs"), [6, 7]);
    }

    #[test]
    fn the_readers_language_rule_gives_the_lens_its_grammar() {
        assert_eq!(
            uses_of_a(&Highlighter::default(), "a.inc"),
            [2, 6, 7],
            "plain words reach the whole file"
        );
        let rules = vec![("*.inc".to_owned(), "rust".to_owned())];
        assert_eq!(
            uses_of_a(&Highlighter::default().with_rules(rules), "a.inc"),
            [6, 7]
        );
    }
}
