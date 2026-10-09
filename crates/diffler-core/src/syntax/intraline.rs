//! Intra-line emphasis from an AST diff (syndiff). [`crate::pairing`] holds
//! the textual engine we fall back to.

use std::ops::Range;

use syndiff::{SyntaxDiffOptions, build_tree, diff_trees};

use crate::model::{FileDiff, Hunk, LineKind};
use crate::syntax::registry::{LangEntry, LanguageRegistry};
use crate::syntax::{MAX_PARSE_BYTES, line_bounds, parse, split_range_by_line};

type LineEmphasis = Vec<Vec<Range<usize>>>;

/// We bound the AST-diff graph search so a huge rewrite cannot stall the
/// render thread; past it the caller falls back to the textual engine.
const GRAPH_LIMIT: usize = 250_000;

impl LanguageRegistry {
    /// `None` when the file cannot be parsed or the diff exceeds its graph
    /// budget.
    fn line_emphasis(
        entry: &LangEntry,
        old_src: &str,
        new_src: &str,
    ) -> Option<(LineEmphasis, LineEmphasis)> {
        if old_src.len() > MAX_PARSE_BYTES || new_src.len() > MAX_PARSE_BYTES {
            return None;
        }
        // markdown's block tree makes a paragraph one opaque node, so the
        // textual word diff serves prose better
        if entry.name == "markdown" {
            return None;
        }
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

    /// Set emphasis on `file`'s diff lines from an AST diff of both sides.
    /// `mark_reformat_only` also flags paired lines that differ in layout
    /// alone. Returns `false` when the file cannot be diffed this way, so the
    /// caller falls back to the textual engine.
    pub fn syntactic_emphasis(
        entry: Option<&LangEntry>,
        file: &mut FileDiff,
        mark_reformat_only: bool,
    ) -> bool {
        let Some(entry) = entry else {
            return false;
        };
        let emphasis = match (file.old_text.as_deref(), file.new_text.as_deref()) {
            (Some(old), Some(new)) => Self::line_emphasis(entry, old, new),
            _ => None,
        };
        let Some((old_emph, new_emph)) = emphasis else {
            return false;
        };
        let mark_reformat_only = mark_reformat_only && !entry.layout_significant;
        for hunk in &mut file.hunks {
            for line in &mut hunk.lines {
                let ranges = match (line.new_no, line.old_no) {
                    (Some(n), _) => new_emph.get(n as usize - 1),
                    (None, Some(o)) => old_emph.get(o as usize - 1),
                    _ => None,
                };
                line.emphasis =
                    classify_line(line.kind, &line.text, ranges.map_or(&[], Vec::as_slice));
            }
            refine_partial_changes(hunk);
            if mark_reformat_only {
                mark_reformat_pairs(hunk, &old_emph, &new_emph);
            }
        }
        true
    }
}

/// Flag a paired line `reformat_only` when the two differ in whitespace alone
/// and the AST diff found no token changed. We need both checks so a
/// whitespace edit inside a string literal stays a real change.
fn mark_reformat_pairs(hunk: &mut Hunk, old_emph: &LineEmphasis, new_emph: &LineEmphasis) {
    let unchanged = |emph: &LineEmphasis, number: Option<u32>| {
        number
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| emph.get(i as usize))
            .is_some_and(Vec::is_empty)
    };
    let squeezed = |text: &str| text.split_whitespace().collect::<String>();
    for (del_idx, add_idx) in crate::pairing::paired_run_indices(&hunk.lines) {
        let (Some(del), Some(add)) = (hunk.lines.get(del_idx), hunk.lines.get(add_idx)) else {
            continue;
        };
        if unchanged(old_emph, del.old_no)
            && unchanged(new_emph, add.new_no)
            && squeezed(&del.text) == squeezed(&add.text)
        {
            if let Some(line) = hunk.lines.get_mut(del_idx) {
                line.reformat_only = true;
            }
            if let Some(line) = hunk.lines.get_mut(add_idx) {
                line.reformat_only = true;
            }
        }
    }
}

/// Replace the AST diff's coarse token ranges on a partly changed pair with a
/// word diff of the two lines, so an edit inside a string does not light up
/// the whole string. Unpaired lines render plain, since the AST diff leaves
/// stray fragments on them where it matched new code against old.
fn refine_partial_changes(hunk: &mut Hunk) {
    let pairs = crate::pairing::paired_run_indices(&hunk.lines);
    let paired: std::collections::HashSet<usize> =
        pairs.iter().flat_map(|&(d, a)| [d, a]).collect();
    for (index, line) in hunk.lines.iter_mut().enumerate() {
        if matches!(line.kind, LineKind::Deleted | LineKind::Added) && !paired.contains(&index) {
            line.emphasis = Vec::new();
        }
    }
    for (del_idx, add_idx) in pairs {
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
        let (old_emph, new_emph) = crate::pairing::gated_pair_emphasis(&old, &new);
        if let Some(line) = hunk.lines.get_mut(del_idx) {
            line.emphasis = old_emph;
        }
        if let Some(line) = hunk.lines.get_mut(add_idx) {
            line.emphasis = new_emph;
        }
    }
}

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

/// A line that changed mostly or entirely gets no emphasis, since
/// highlighting almost everything highlights nothing.
fn classify_line(kind: LineKind, text: &str, ranges: &[Range<usize>]) -> Vec<Range<usize>> {
    let _ = kind;
    let ranges = clamp(ranges, text.len());
    if ranges.is_empty() || !crate::pairing::emphasis_is_punctual(text, &ranges) {
        return Vec::new();
    }
    ranges
}

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
        let (_, new_e) =
            LanguageRegistry::line_emphasis(reg.for_path("a.rs").expect("bundled"), old, new)
                .expect("rust parses");
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
        let (_, new_e) =
            LanguageRegistry::line_emphasis(reg.for_path("a.rs").expect("bundled"), old, new)
                .expect("rust parses");
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
            blobs: crate::model::BlobIds::default(),
        };
        assert!(crate::highlight::Highlighter::default().syntactic_emphasis(&mut file, false));
        let added = &file.hunks[0].lines[1];
        assert!(!added.emphasis.is_empty(), "the changed line is emphasized");
        let covered: String = added
            .emphasis
            .iter()
            .filter_map(|r| new_line.get(r.clone()))
            .collect();
        assert!(
            covered.contains("EXTRA"),
            "covers the insertion: {covered:?}"
        );
        assert!(
            !covered.contains("foo"),
            "the unchanged prefix is not emphasized: {covered:?}"
        );
    }

    fn reformat_flagged(path: &str, old: &str, new: &str) -> Vec<String> {
        let mut file = crate::model::FileDiff {
            path: path.into(),
            old_path: None,
            status: crate::model::FileStatus::Modified,
            binary: false,
            old_text: Some(old.into()),
            new_text: Some(new.into()),
            hunks: crate::diffalgo::histogram_hunks(old, new, path, 3, true),
            hashes: crate::model::HashCache::default(),
            blobs: crate::model::BlobIds::default(),
        };
        assert!(crate::highlight::Highlighter::default().syntactic_emphasis(&mut file, true));
        file.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.reformat_only)
            .map(|l| l.text.clone())
            .collect()
    }

    #[test]
    fn structural_mode_marks_a_pure_reindent_pair_reformat_only() {
        let flagged = reformat_flagged(
            "a.rs",
            "fn f() {\n    let x = compute();\n}\n",
            "fn f() {\n        let x = compute();\n}\n",
        );
        assert_eq!(
            flagged,
            ["    let x = compute();", "        let x = compute();"]
        );
    }

    #[test]
    fn structural_mode_leaves_a_real_change_unflagged() {
        let flagged = reformat_flagged(
            "a.rs",
            "fn f() {\n    let x = 1;\n}\n",
            "fn f() {\n    let x = 2;\n}\n",
        );
        assert!(flagged.is_empty(), "{flagged:?}");
    }

    #[test]
    fn structural_mode_keeps_whitespace_inside_a_string_a_change() {
        let flagged = reformat_flagged(
            "a.rs",
            "fn f() {\n    let s = \"a b\";\n}\n",
            "fn f() {\n    let s = \"a  b\";\n}\n",
        );
        assert!(flagged.is_empty(), "{flagged:?}");
    }

    #[test]
    fn structural_mode_never_flags_a_reindent_where_layout_is_syntax() {
        let python = reformat_flagged(
            "a.py",
            "x = 1\nif c:\n    pass\ny = 2\n",
            "x = 1\nif c:\n    pass\n    y = 2\n",
        );
        assert!(python.is_empty(), "moving into the block: {python:?}");
        let yaml = reformat_flagged("a.yaml", "a:\n  b: 1\nc: 2\n", "a:\n  b: 1\n  c: 2\n");
        assert!(yaml.is_empty(), "nesting a key: {yaml:?}");
    }

    #[test]
    fn wholly_new_code_never_carries_fragment_emphasis() {
        use crate::model::{DiffLine, FileDiff, FileStatus, Hunk, HunkId, LineKind};
        let old_src = "function keep(path: string): string {\n    return path;\n}\n";
        let added = [
            "function fresh(path: string): string {",
            "    if (!path) {",
            "        return \"missing\";",
            "    }",
            "    return path;",
            "}",
        ];
        let new_src = format!("{old_src}\n{}\n", added.join("\n"));
        let lines = added
            .iter()
            .enumerate()
            .map(|(i, text)| {
                DiffLine::new(
                    LineKind::Added,
                    None,
                    Some(5 + i as u32),
                    (*text).to_owned(),
                )
            })
            .collect();
        let mut file = FileDiff {
            path: "a.ts".into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            old_text: Some(old_src.to_owned()),
            new_text: Some(new_src),
            hunks: vec![Hunk {
                id: HunkId("h".into()),
                old_start: 3,
                old_lines: 0,
                new_start: 5,
                new_lines: 6,
                context: String::new(),
                lines,
            }],
            hashes: crate::model::HashCache::default(),
            blobs: crate::model::BlobIds::default(),
        };
        assert!(crate::highlight::Highlighter::default().syntactic_emphasis(&mut file, false));
        for line in &file.hunks[0].lines {
            assert!(
                line.emphasis.is_empty(),
                "no pair, no emphasis: {:?} got {:?}",
                line.text,
                line.emphasis
            );
        }
    }

    #[test]
    fn tsx_wrap_and_reindent_marks_only_real_changes() {
        let reg = LanguageRegistry::build();
        let old = "<Form>\n  <Button onClick={onApply}>Apply</Button>\n</Form>\n";
        let new = "{(values) => (\n  <Form>\n    <Button onClick={() => apply(values)}>Apply</Button>\n  </Form>\n)}\n";
        let (_, new_e) =
            LanguageRegistry::line_emphasis(reg.for_path("a.tsx").expect("bundled"), old, new)
                .expect("tsx parses");
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
    fn classify_unchanged_line_gets_no_emphasis() {
        let emph = classify_line(LineKind::Added, "    <Form>", &[]);
        assert!(emph.is_empty());
    }

    #[test]
    fn classify_whole_line_change_keeps_background_without_emphasis() {
        let text = "    let entirely_new = compute();";
        let ranges = [4..7, 8..20, 21..22, 23..text.len()];
        let emph = classify_line(LineKind::Added, text, &ranges);
        assert!(
            emph.is_empty(),
            "no char emphasis when the whole line changed"
        );
    }

    #[test]
    fn classify_mostly_changed_line_drops_emphasis() {
        let text = "    let entirely_new = compute();";
        let ranges = [4..7, 8..20, 23..30];
        let emph = classify_line(LineKind::Added, text, &ranges);
        assert!(emph.is_empty(), "{emph:?}");
    }

    #[test]
    fn classify_partial_change_keeps_emphasis() {
        let text = "    let x = 2;";
        let changed = 12..13;
        let emph = classify_line(LineKind::Added, text, std::slice::from_ref(&changed));
        assert_eq!(emph.len(), 1);
        assert_eq!(emph[0], changed);
    }
}
