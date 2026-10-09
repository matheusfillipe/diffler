//! Selectable line-diff algorithms. libgit2 has no histogram, so we run
//! `Histogram` and `Structural` through imara-diff.

use std::collections::HashMap;

use imara_diff::{Algorithm, Diff, InternedInput};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::{DiffLine, Hunk, HunkId, LineKind, disambiguated_hunk_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffAlgorithm {
    #[default]
    Myers,
    Minimal,
    Patience,
    Histogram,
    /// Histogram, plus dimming paired lines that differ in layout alone.
    Structural,
}

impl DiffAlgorithm {
    pub const ALL: [Self; 5] = [
        Self::Myers,
        Self::Minimal,
        Self::Patience,
        Self::Histogram,
        Self::Structural,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Myers => "myers",
            Self::Minimal => "minimal",
            Self::Patience => "patience",
            Self::Histogram => "histogram",
            Self::Structural => "structural",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == value)
    }

    pub const fn is_imara(self) -> bool {
        matches!(self, Self::Histogram | Self::Structural)
    }
}

impl std::fmt::Display for DiffAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for DiffAlgorithm {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DiffAlgorithm {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name).ok_or_else(|| {
            let names = Self::ALL.map(Self::as_str).join(", ");
            serde::de::Error::custom(format!(
                "unknown diff algorithm `{name}`, expected one of {names}"
            ))
        })
    }
}

/// One value for every backend we open, so the review's backend and the
/// workers' never drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffSettings {
    pub context_lines: u32,
    pub algorithm: DiffAlgorithm,
    pub indent_heuristic: bool,
}

impl Default for DiffSettings {
    fn default() -> Self {
        Self {
            context_lines: crate::git::DEFAULT_CONTEXT_LINES,
            algorithm: DiffAlgorithm::default(),
            indent_heuristic: crate::git::DEFAULT_INDENT_HEURISTIC,
        }
    }
}

impl DiffSettings {
    pub fn with_context(context_lines: u32) -> Self {
        Self {
            context_lines,
            ..Self::default()
        }
    }
}

/// Histogram hunks of `old` vs `new`, merged the way git merges nearby hunks.
/// We seed ids like [`crate::git`] so staging finds the hunk it shows.
pub fn histogram_hunks(
    old: &str,
    new: &str,
    file_path: &str,
    context: u32,
    indent_heuristic: bool,
) -> Vec<Hunk> {
    let input = InternedInput::new(old, new);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    if indent_heuristic {
        diff.postprocess_lines(&input);
    } else {
        diff.postprocess_no_heuristic(&input);
    }

    let before_len = input.before.len() as u32;
    let raw: Vec<imara_diff::Hunk> = diff.hunks().collect();
    if raw.is_empty() {
        return Vec::new();
    }

    let two_context = context.saturating_mul(2);
    let mut groups: Vec<Vec<imara_diff::Hunk>> = Vec::new();
    let mut pos = 0u32;
    for hunk in raw {
        let starts_new = groups.is_empty() || hunk.before.start.saturating_sub(pos) > two_context;
        if starts_new {
            groups.push(Vec::new());
        }
        pos = hunk.before.end;
        if let Some(group) = groups.last_mut() {
            group.push(hunk);
        }
    }

    let mut seen: HashMap<HunkId, usize> = HashMap::new();
    let mut heading = FuncHeading::default();
    groups
        .into_iter()
        .map(|group| {
            build_hunk(
                &group,
                &input,
                file_path,
                context,
                before_len,
                &mut seen,
                &mut heading,
            )
        })
        .collect()
}

fn line_text(input: &InternedInput<&str>, token: imara_diff::Token) -> String {
    input.interner[token]
        .trim_end_matches(['\n', '\r'])
        .to_owned()
}

/// libgit2's default funcname rule: the nearest old-side line above the hunk
/// that opens with an ASCII letter, `_` or `$`, cut to 80 bytes. Hunks arrive
/// in file order, so we scan only the rows since the previous hunk.
#[derive(Default)]
struct FuncHeading {
    scanned: u32,
    text: String,
}

impl FuncHeading {
    const MAX_BYTES: usize = 80;

    fn above(&mut self, input: &InternedInput<&str>, row: u32) -> String {
        let found = (self.scanned..row)
            .rev()
            .filter_map(|idx| input.before.get(idx as usize))
            .map(|&token| input.interner[token].trim_end())
            .find(|line| {
                line.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$')
            });
        if let Some(line) = found {
            let mut end = line.len().min(Self::MAX_BYTES);
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line[..end].trim_end().clone_into(&mut self.text);
        }
        self.scanned = self.scanned.max(row);
        self.text.clone()
    }
}

fn push_context_lines(
    lines: &mut Vec<DiffLine>,
    input: &InternedInput<&str>,
    old_from: u32,
    old_to: u32,
    new_from: u32,
) {
    for offset in 0..old_to.saturating_sub(old_from) {
        let old_idx = old_from + offset;
        let new_idx = new_from + offset;
        let Some(&token) = input.before.get(old_idx as usize) else {
            continue;
        };
        lines.push(DiffLine::new(
            LineKind::Context,
            Some(old_idx + 1),
            Some(new_idx + 1),
            line_text(input, token),
        ));
    }
}

fn build_hunk(
    group: &[imara_diff::Hunk],
    input: &InternedInput<&str>,
    file_path: &str,
    context: u32,
    before_len: u32,
    seen: &mut HashMap<HunkId, usize>,
    heading: &mut FuncHeading,
) -> Hunk {
    let first = group.first().unwrap_or(&imara_diff::Hunk::NONE);
    let last = group.last().unwrap_or(&imara_diff::Hunk::NONE);

    let lead_start = first.before.start.saturating_sub(context);
    let lead_len = first.before.start - lead_start;
    let after_lead_start = first.after.start.saturating_sub(lead_len);
    let tail_end = last.before.end.saturating_add(context).min(before_len);

    let mut lines = Vec::new();
    push_context_lines(
        &mut lines,
        input,
        lead_start,
        first.before.start,
        after_lead_start,
    );

    for (index, hunk) in group.iter().enumerate() {
        for old_idx in hunk.before.start..hunk.before.end {
            let Some(&token) = input.before.get(old_idx as usize) else {
                continue;
            };
            lines.push(DiffLine::new(
                LineKind::Deleted,
                Some(old_idx + 1),
                None,
                line_text(input, token),
            ));
        }
        for new_idx in hunk.after.start..hunk.after.end {
            let Some(&token) = input.after.get(new_idx as usize) else {
                continue;
            };
            lines.push(DiffLine::new(
                LineKind::Added,
                None,
                Some(new_idx + 1),
                line_text(input, token),
            ));
        }
        if let Some(next) = group.get(index + 1) {
            push_context_lines(
                &mut lines,
                input,
                hunk.before.end,
                next.before.start,
                hunk.after.end,
            );
        }
    }
    push_context_lines(&mut lines, input, last.before.end, tail_end, last.after.end);

    let old_lines = lines.iter().filter(|l| l.kind != LineKind::Added).count() as u32;
    let new_lines = lines.iter().filter(|l| l.kind != LineKind::Deleted).count() as u32;
    // git's header gives an empty side the line before the change
    let start = |index: u32, len: u32| if len == 0 { index } else { index + 1 };
    let id = disambiguated_hunk_id(file_path, &lines, seen);
    Hunk {
        id,
        old_start: start(lead_start, old_lines),
        old_lines,
        new_start: start(after_lead_start, new_lines),
        new_lines,
        context: heading.above(input, lead_start),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn algorithm_names_round_trip() {
        for algo in DiffAlgorithm::ALL {
            assert_eq!(DiffAlgorithm::parse(algo.as_str()), Some(algo));
            let json = serde_json::to_string(&algo).expect("serialize");
            assert_eq!(json, format!("\"{}\"", algo.as_str()));
            assert_eq!(
                serde_json::from_str::<DiffAlgorithm>(&json).expect("deserialize"),
                algo
            );
        }
        assert_eq!(DiffAlgorithm::parse("bogus"), None);
        assert!(serde_json::from_str::<DiffAlgorithm>("\"bogus\"").is_err());
    }

    #[test]
    fn default_algorithm_is_myers() {
        assert_eq!(DiffAlgorithm::default(), DiffAlgorithm::Myers);
    }

    #[test]
    fn single_line_change_yields_one_hunk_with_context() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nX\nd\ne\n";
        let hunks = histogram_hunks(old, new, "f.txt", 1, true);
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        assert_eq!(h.old_start, 2);
        assert_eq!(h.old_lines, 3);
        assert_eq!(h.new_start, 2);
        assert_eq!(h.new_lines, 3);
        let kinds: Vec<_> = h.lines.iter().map(|l| (l.kind, l.text.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (LineKind::Context, "b"),
                (LineKind::Deleted, "c"),
                (LineKind::Added, "X"),
                (LineKind::Context, "d"),
            ]
        );
    }

    #[test]
    fn nearby_changes_merge_into_one_hunk() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "A\nb\nc\nD\ne\n";
        let hunks = histogram_hunks(old, new, "f.txt", 3, true);
        assert_eq!(hunks.len(), 1, "the two edits merge: {hunks:?}");
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[0].old_lines, 5);
    }

    #[test]
    fn distant_changes_stay_separate_hunks() {
        let mut old = vec!["a".to_owned()];
        old.extend((0..20).map(|i| format!("ctx{i}")));
        old.push("z".to_owned());
        let mut new = old.clone();
        new[0] = "A".to_owned();
        let last = new.len() - 1;
        new[last] = "Z".to_owned();
        let old_text = format!("{}\n", old.join("\n"));
        let new_text = format!("{}\n", new.join("\n"));
        let hunks = histogram_hunks(&old_text, &new_text, "f.txt", 3, true);
        assert_eq!(hunks.len(), 2, "far-apart edits stay separate: {hunks:?}");
    }

    #[test]
    fn leading_and_trailing_context_clip_to_file_bounds() {
        let old = "a\nb\n";
        let new = "A\nb\n";
        let hunks = histogram_hunks(old, new, "f.txt", 3, true);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[0].old_lines, 2);
    }

    #[test]
    fn no_context_only_shows_changed_lines() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let hunks = histogram_hunks(old, new, "f.txt", 0, true);
        assert_eq!(hunks.len(), 1);
        let kinds: Vec<_> = hunks[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![LineKind::Deleted, LineKind::Added]);
    }

    #[test]
    fn an_empty_side_starts_at_the_line_before_like_git() {
        let insert = histogram_hunks("a\nb\n", "a\nX\nb\n", "f.txt", 0, true);
        assert_eq!(
            (
                insert[0].old_start,
                insert[0].old_lines,
                insert[0].new_start
            ),
            (1, 0, 2)
        );
        let delete = histogram_hunks("a\nX\nb\n", "a\nb\n", "f.txt", 0, true);
        assert_eq!(
            (
                delete[0].old_start,
                delete[0].new_start,
                delete[0].new_lines
            ),
            (2, 1, 0)
        );
    }

    #[test]
    fn hunk_ids_are_stable_for_identical_input() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let first = histogram_hunks(old, new, "f.txt", 1, true);
        let second = histogram_hunks(old, new, "f.txt", 1, true);
        assert_eq!(first[0].id, second[0].id);
    }

    // the hunk_context expectations below match `git diff` on the same input

    #[test]
    fn hunk_context_finds_the_enclosing_function() {
        let old = "def parse_config():\n    a = 1\n    b = 2\n    c = 3\n";
        let new = "def parse_config():\n    a = 1\n    b = 20\n    c = 3\n";
        let hunks = histogram_hunks(old, new, "f.py", 0, true);
        assert_eq!(hunks[0].context, "def parse_config():");
    }

    #[test]
    fn hunk_context_skips_a_same_indent_sibling() {
        let old = "def parse_config():\n    a = 1\n    b = 2\n";
        let new = "def parse_config():\n    a = 1\n    b = 20\n";
        let hunks = histogram_hunks(old, new, "f.py", 0, true);
        assert_eq!(hunks[0].context, "def parse_config():");
    }

    #[test]
    fn hunk_context_climbs_to_the_outermost_scope() {
        let old = "class Foo:\n    def bar():\n        a = 1\n        b = 2\n";
        let new = "class Foo:\n    def bar():\n        a = 1\n        b = 20\n";
        let hunks = histogram_hunks(old, new, "f.py", 0, true);
        assert_eq!(hunks[0].context, "class Foo:");
    }

    #[test]
    fn hunk_context_prefers_the_nearest_top_level_definition() {
        let old =
            "def first():\n    pass\n\ndef second():\n    if true:\n        x = 1\n        y = 2\n";
        let new = "def first():\n    pass\n\ndef second():\n    if true:\n        x = 1\n        y = 20\n";
        let hunks = histogram_hunks(old, new, "f.py", 0, true);
        assert_eq!(hunks[0].context, "def second():");
    }

    #[test]
    fn hunk_context_for_an_insertion_reads_the_enclosing_function() {
        let old = "def outer():\n    a = 1\n    b = 2\n";
        let new = "def outer():\n    a = 1\n    newline = 99\n    b = 2\n";
        let hunks = histogram_hunks(old, new, "f.py", 0, true);
        assert_eq!(hunks[0].context, "def outer():");
    }

    #[test]
    fn hunk_context_for_a_top_level_change_names_the_definition_above() {
        let old = "fn top() {\n    1;\n}\n\nfn next() {\n    2;\n}\n";
        let new = "fn top() {\n    1;\n}\n\nfn renamed() {\n    2;\n}\n";
        let hunks = histogram_hunks(old, new, "f.rs", 0, true);
        assert_eq!(hunks[0].context, "fn top() {");
    }

    #[test]
    fn hunk_context_reads_above_the_leading_context_lines() {
        let old = "fn a() {\n    1;\n    2;\n}\n";
        let new = "fn a() {\n    10;\n    2;\n}\n";
        let hunks = histogram_hunks(old, new, "f.rs", 3, true);
        assert_eq!(hunks[0].context, "", "the definition is a context line");
    }

    #[test]
    fn a_later_hunk_keeps_the_heading_when_no_definition_lies_between() {
        let mut old = String::from("fn only() {\n");
        old.extend((0..20).map(|i| format!("    line{i};\n")));
        old.push_str("}\n");
        let new = old
            .replace("line2;", "LINE2;")
            .replace("line17;", "LINE17;");
        let hunks = histogram_hunks(&old, &new, "f.rs", 1, true);
        let contexts: Vec<_> = hunks.iter().map(|h| h.context.as_str()).collect();
        assert_eq!(contexts, ["fn only() {", "fn only() {"]);
    }

    #[test]
    fn hunk_context_is_cut_to_git_s_80_bytes_on_a_char_boundary() {
        let name = "é".repeat(60);
        let old = format!("fn {name}() {{\n    a;\n    b;\n}}\n");
        let new = old.replace("    b;", "    B;");
        let hunks = histogram_hunks(&old, &new, "f.rs", 0, true);
        assert_eq!(hunks[0].context, format!("fn {}", "é".repeat(38)));
    }
}
