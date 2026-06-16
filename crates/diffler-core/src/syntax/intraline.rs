//! Char-precise intra-line change emphasis driven by an AST diff (syndiff).
//! Unlike the textual engine in [`crate::pairing`], reindentation and block
//! wrapping are not flagged — only the byte ranges that differ structurally
//! are emphasized — so a reformatted or re-wrapped block highlights just the
//! tokens that actually changed.

use std::ops::Range;

use syndiff::{SyntaxDiffOptions, build_tree, diff_trees};

use crate::model::FileDiff;
use crate::syntax::registry::LanguageRegistry;
use crate::syntax::{MAX_PARSE_BYTES, line_bounds, parse, split_range_by_line};

/// Emphasis byte ranges per line (one inner vec per source line).
type LineEmphasis = Vec<Vec<Range<usize>>>;

/// Bounds the AST-diff graph search so a huge, heavily rewritten file cannot
/// stall the render thread; beyond it `diff_trees` returns `None` and the
/// caller falls back to the textual engine. Well above any normal diff.
const GRAPH_LIMIT: usize = 250_000;

impl LanguageRegistry {
    /// Per-line emphasis byte ranges for both sides, from an AST diff of the
    /// full old/new content. `None` (caller falls back to the textual engine)
    /// when the language is unsupported, content is too large, parsing fails,
    /// or the diff exceeds its graph budget.
    pub fn line_emphasis(
        &self,
        path: &str,
        old_src: &str,
        new_src: &str,
    ) -> Option<(LineEmphasis, LineEmphasis)> {
        if old_src.len() > MAX_PARSE_BYTES || new_src.len() > MAX_PARSE_BYTES {
            return None;
        }
        let entry = self.for_path(path)?;
        let old_ts = parse(entry, old_src)?;
        let new_ts = parse(entry, new_src)?;
        let old_tree = build_tree(old_ts.walk(), old_src);
        let new_tree = build_tree(new_ts.walk(), new_src);
        let options = SyntaxDiffOptions {
            graph_limit: GRAPH_LIMIT,
        };
        let (old_ranges, new_ranges) = diff_trees(&old_tree, &new_tree, None, None, Some(options))?;
        Some((
            per_line_emphasis(old_src, &old_ranges),
            per_line_emphasis(new_src, &new_ranges),
        ))
    }

    /// Set char-precise emphasis on `file`'s diff lines from the AST diff.
    /// Returns `false` when the syntactic engine is unavailable, so the caller
    /// can fall back to the textual engine.
    pub fn syntactic_emphasis(&self, file: &mut FileDiff) -> bool {
        let emphasis = match (file.old_text.as_deref(), file.new_text.as_deref()) {
            (Some(old), Some(new)) => self.line_emphasis(&file.path, old, new),
            _ => None,
        };
        let Some((old_emph, new_emph)) = emphasis else {
            return false;
        };
        for hunk in &mut file.hunks {
            for line in &mut hunk.lines {
                let ranges = match (line.new_no, line.old_no) {
                    (Some(n), _) => new_emph.get(n as usize - 1),
                    (None, Some(o)) => old_emph.get(o as usize - 1),
                    _ => None,
                };
                line.emphasis = ranges
                    .map(|r| clamp(r, line.text.len()))
                    .unwrap_or_default();
            }
        }
        true
    }
}

/// Map whole-file changed byte ranges to per-line, within-line ranges. Lines
/// whose entire content changed are cleared: a fully added/removed/rewritten
/// line has nothing to distinguish, so the +/- background already says it all
/// and char emphasis there is just noise.
fn per_line_emphasis(src: &str, ranges: &[Range<usize>]) -> LineEmphasis {
    let bounds = line_bounds(src);
    let starts: Vec<usize> = bounds.iter().map(|&(s, _)| s).collect();
    let mut out = vec![Vec::new(); bounds.len()];
    for r in ranges {
        split_range_by_line(&bounds, &starts, r, |li, rr| {
            if let Some(v) = out.get_mut(li) {
                v.push(rr);
            }
        });
    }
    for (line_ranges, &(s, e)) in out.iter_mut().zip(&bounds) {
        if whole_line_changed(src.get(s..e).unwrap_or(""), line_ranges) {
            line_ranges.clear();
        }
    }
    out
}

/// True when every non-whitespace byte of the line is emphasized (gaps fall
/// only on whitespace) — i.e. the entire content changed.
fn whole_line_changed(text: &str, ranges: &[Range<usize>]) -> bool {
    let mut any_content = false;
    for (i, &b) in text.as_bytes().iter().enumerate() {
        if b == b' ' || b == b'\t' {
            continue;
        }
        any_content = true;
        if !ranges.iter().any(|r| r.start <= i && i < r.end) {
            return false;
        }
    }
    any_content
}

/// Clip ranges to the line's length and drop any that become empty.
fn clamp(ranges: &[Range<usize>], len: usize) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter_map(|r| {
            let end = r.end.min(len);
            (r.start < end).then_some(r.start..end)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_with(src: &str, needle: &str) -> usize {
        src.lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?}"))
    }

    #[test]
    fn pure_reindent_is_not_emphasized() {
        let reg = LanguageRegistry::build();
        let old = "fn f() {\n    let x = compute();\n    use_it(x);\n}\n";
        let new = "fn f() {\n        let x = compute();\n        use_it(x);\n}\n";
        let (_, new_e) = reg.line_emphasis("a.rs", old, new).expect("rust parses");
        assert!(
            new_e.iter().all(Vec::is_empty),
            "reindentation must produce no emphasis, got {new_e:?}"
        );
    }

    #[test]
    fn a_real_token_change_is_emphasized() {
        let reg = LanguageRegistry::build();
        let old = "fn f() {\n    let x = 1;\n}\n";
        let new = "fn f() {\n    let x = 2;\n}\n";
        let (_, new_e) = reg.line_emphasis("a.rs", old, new).expect("rust parses");
        let changed = line_with(new, "let x = 2");
        let signature = line_with(new, "fn f()");
        assert!(!new_e[changed].is_empty(), "the changed line is emphasized");
        assert!(
            new_e[signature].is_empty(),
            "the unchanged signature line is not"
        );
    }

    #[test]
    fn tsx_wrap_and_reindent_marks_only_real_changes() {
        let reg = LanguageRegistry::build();
        let old = "<Form>\n  <Button onClick={onApply}>Apply</Button>\n</Form>\n";
        let new = "{(values) => (\n  <Form>\n    <Button onClick={() => apply(values)}>Apply</Button>\n  </Form>\n)}\n";
        let (_, new_e) = reg.line_emphasis("a.tsx", old, new).expect("tsx parses");
        let reindented = line_with(new, "<Form>");
        let changed = line_with(new, "apply(values)");
        assert!(
            new_e[reindented].is_empty(),
            "a reindented-but-identical line is not emphasized, got {:?}",
            new_e[reindented]
        );
        assert!(
            !new_e[changed].is_empty(),
            "the structurally changed line is emphasized"
        );
    }

    #[test]
    fn a_fully_added_line_is_not_char_emphasized() {
        let reg = LanguageRegistry::build();
        let old = "fn f() {\n}\n";
        let new = "fn f() {\n    let entirely_new = compute_something();\n}\n";
        let (_, new_e) = reg.line_emphasis("a.rs", old, new).expect("rust parses");
        let added = line_with(new, "entirely_new");
        assert!(
            new_e[added].is_empty(),
            "a wholly-new line has nothing to distinguish, got {:?}",
            new_e[added]
        );
    }

    #[test]
    fn unsupported_language_returns_none() {
        let reg = LanguageRegistry::build();
        assert!(reg.line_emphasis("a.zzz", "a\n", "b\n").is_none());
    }
}
