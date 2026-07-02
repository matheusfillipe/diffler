//! Char-precise intra-line change emphasis driven by an AST diff (syndiff).
//! Unlike the textual engine in [`crate::pairing`], reindentation and block
//! wrapping are not flagged — only the byte ranges that differ structurally
//! are emphasized — so a reformatted or re-wrapped block highlights just the
//! tokens that actually changed.

use std::ops::Range;

use syndiff::{SyntaxDiffOptions, build_tree, diff_trees};

use crate::model::{FileDiff, Hunk, LineKind};
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
    fn line_emphasis(
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
                let (moved, emphasis) =
                    classify_line(line.kind, &line.text, ranges.map_or(&[], Vec::as_slice));
                line.moved = moved;
                line.emphasis = emphasis;
            }
            refine_partial_changes(hunk);
        }
        true
    }
}

/// Where the AST diff flagged a *partial* line change (some token ranges, not
/// the whole line and not a reformat), replace the coarse token ranges with a
/// grapheme-level diff of the paired lines, so only the characters that actually
/// changed are emphasized (an edit inside a string scalar shouldn't light up the
/// whole scalar). Whole-line changes and reformat-only lines keep the AST result.
fn refine_partial_changes(hunk: &mut Hunk) {
    for (del_idx, add_idx) in crate::pairing::paired_run_indices(&hunk.lines) {
        let partial = hunk
            .lines
            .get(del_idx)
            .is_some_and(|l| !l.emphasis.is_empty())
            || hunk
                .lines
                .get(add_idx)
                .is_some_and(|l| !l.emphasis.is_empty());
        if !partial {
            continue;
        }
        let (Some(old), Some(new)) = (
            hunk.lines.get(del_idx).map(|l| l.text.clone()),
            hunk.lines.get(add_idx).map(|l| l.text.clone()),
        ) else {
            continue;
        };
        let (old_emph, new_emph) = crate::diff::intraline(&old, &new);
        if let Some(line) = hunk.lines.get_mut(del_idx) {
            line.emphasis = old_emph;
        }
        if let Some(line) = hunk.lines.get_mut(add_idx) {
            line.emphasis = new_emph;
        }
    }
}

/// Map whole-file changed byte ranges to the raw per-line, within-line ranges.
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
    out
}

/// Classify an added/deleted `line` from its raw changed byte `ranges`:
/// - no change → a reindent/move: `(moved = true, no emphasis)`, a thin rail;
/// - the whole content changed → `(false, no emphasis)`, a full +/- background;
/// - some tokens changed → `(false, those ranges)`, background plus emphasis.
fn classify_line(kind: LineKind, text: &str, ranges: &[Range<usize>]) -> (bool, Vec<Range<usize>>) {
    let ranges = clamp(ranges, text.len());
    let changed = matches!(kind, LineKind::Added | LineKind::Deleted);
    if ranges.is_empty() {
        return (changed, Vec::new());
    }
    if whole_line_changed(text, &ranges) {
        return (false, Vec::new());
    }
    (false, ranges)
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
    fn in_string_edit_is_char_precise_not_whole_token() {
        use crate::model::{DiffLine, FileDiff, FileStatus, Hunk, HunkId, LineKind};
        let old_line = "fn f() { let s = \"foo/bar\"; }";
        let new_line = "fn f() { let s = \"foo/EXTRA/bar\"; }";
        let mut file = FileDiff {
            path: "a.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            old_text: Some(format!("{old_line}\n")),
            new_text: Some(format!("{new_line}\n")),
            hunks: vec![Hunk {
                id: HunkId("h".into()),
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                context: String::new(),
                lines: vec![
                    DiffLine::new(LineKind::Deleted, Some(1), None, old_line.to_owned()),
                    DiffLine::new(LineKind::Added, None, Some(1), new_line.to_owned()),
                ],
            }],
            hashes: crate::model::HashCache::default(),
        };
        assert!(LanguageRegistry::build().syntactic_emphasis(&mut file));
        let added = &file.hunks[0].lines[1];
        assert!(!added.emphasis.is_empty(), "the changed line is emphasized");
        let covered: String = added
            .emphasis
            .iter()
            .filter_map(|r| new_line.get(r.clone()))
            .collect();
        // only the inserted run is emphasized, not the whole "foo/EXTRA/bar" token
        assert!(
            covered.contains("EXTRA"),
            "covers the insertion: {covered:?}"
        );
        assert!(
            !covered.contains("foo"),
            "the unchanged prefix is not emphasized: {covered:?}"
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
    fn unsupported_language_returns_none() {
        let reg = LanguageRegistry::build();
        assert!(reg.line_emphasis("a.zzz", "a\n", "b\n").is_none());
    }

    #[test]
    fn classify_reindented_line_is_a_move() {
        // an added/deleted line with no changed bytes only moved/reindented
        let (moved, emph) = classify_line(LineKind::Added, "    <Form>", &[]);
        assert!(moved, "reindent/move is flagged");
        assert!(emph.is_empty());
    }

    #[test]
    fn classify_whole_line_change_keeps_background_without_emphasis() {
        // every non-whitespace byte changed -> full +/- bg, no char emphasis
        let text = "    let entirely_new = compute();";
        let ranges = [4..7, 8..20, 21..22, 23..text.len()];
        let (moved, emph) = classify_line(LineKind::Added, text, &ranges);
        assert!(!moved, "a wholly-changed line is a real change, not a move");
        assert!(
            emph.is_empty(),
            "no char emphasis when the whole line changed"
        );
    }

    #[test]
    fn classify_partial_change_keeps_emphasis() {
        // only `2` changed in `    let x = 2;`
        let text = "    let x = 2;";
        let changed = 12..13;
        let (moved, emph) = classify_line(LineKind::Added, text, std::slice::from_ref(&changed));
        assert!(!moved);
        assert_eq!(emph.len(), 1);
        assert_eq!(emph[0], changed);
    }

    #[test]
    fn classify_context_line_is_never_moved() {
        let (moved, emph) = classify_line(LineKind::Context, "    unchanged", &[]);
        assert!(!moved);
        assert!(emph.is_empty());
    }
}
