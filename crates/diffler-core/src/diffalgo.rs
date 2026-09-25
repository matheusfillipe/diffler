//! Selectable line-diff algorithms, shared by every diff source. `Myers`,
//! `Minimal` and `Patience` are git2's own (see [`crate::git`]); `Histogram`
//! and `Structural` (which layers reformat detection on top, in
//! [`crate::syntax::intraline`]) run through imara-diff, since libgit2 has no
//! histogram implementation.

use std::collections::HashMap;

use imara_diff::{Algorithm, Diff, InternedInput};
use serde::de::IntoDeserializer as _;
use serde::{Deserialize, Serialize};

use crate::model::{DiffLine, Hunk, HunkId, LineKind, disambiguated_hunk_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffAlgorithm {
    #[default]
    Myers,
    Minimal,
    Patience,
    Histogram,
    /// The histogram line diff, plus: a paired deleted/added line that only
    /// reformats the same tokens renders dimmed instead of red/green.
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

    /// The config/display name, exactly the string `#[serde(rename_all)]`
    /// gives this variant, so it can never drift from [`Self::parse`].
    pub fn as_str(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    pub fn parse(value: &str) -> Option<Self> {
        let de: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
            value.into_deserializer();
        Self::deserialize(de).ok()
    }

    /// Whether this algorithm runs through imara-diff instead of git2, since
    /// libgit2 has no histogram implementation.
    pub const fn is_imara(self) -> bool {
        matches!(self, Self::Histogram | Self::Structural)
    }
}

impl std::fmt::Display for DiffAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// The line-diff context, algorithm and indent heuristic every diff source
/// honours, threaded through construction so the review's long-lived backend
/// and every worker that opens a fresh one read the same values and can
/// never drift apart.
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
    /// [`Self::default`] with a custom number of context lines.
    pub fn with_context(context_lines: u32) -> Self {
        Self {
            context_lines,
            ..Self::default()
        }
    }
}

/// Line hunks of `old` vs `new` computed by imara-diff's histogram algorithm,
/// grouped with `context` unchanged lines around each change the way git
/// itself merges nearby hunks together (mirrors imara-diff's own
/// `unified_diff` grouping rule, without needing its text printer).
/// `file_path` seeds the hunk ids the same way [`crate::git`] does, so
/// staging can find the hunk it shows regardless of which algorithm produced
/// it, as long as both read the current algorithm.
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
    groups
        .into_iter()
        .map(|group| build_hunk(&group, &input, file_path, context, before_len, &mut seen))
        .collect()
}

fn line_text(input: &InternedInput<&str>, token: imara_diff::Token) -> String {
    input.interner[token]
        .trim_end_matches(['\n', '\r'])
        .to_owned()
}

fn leading_whitespace(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

fn looks_like_a_definition(line: &str) -> bool {
    line.trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
}

/// git's generic funcname heuristic (used when no per-language driver
/// applies): scanning the old side upward from `start_row`, the nearest line
/// whose indentation keeps dropping and that starts with a letter, `_` or
/// `$`, stopping once indentation reaches zero or the file start.
fn nearest_function_context(input: &InternedInput<&str>, start_row: u32) -> String {
    let mut min_indent = input
        .before
        .get(start_row as usize)
        .map_or(usize::MAX, |&token| {
            leading_whitespace(&line_text(input, token))
        });
    let mut best = String::new();
    for idx in (0..start_row).rev() {
        let Some(&token) = input.before.get(idx as usize) else {
            continue;
        };
        let text = line_text(input, token);
        if !looks_like_a_definition(&text) {
            continue;
        }
        let indent = leading_whitespace(&text);
        if indent < min_indent {
            min_indent = indent;
            best = text;
            if min_indent == 0 {
                break;
            }
        }
    }
    best
}

/// Append context lines for the unchanged old-side span `old_from..old_to`,
/// whose new-side counterpart starts at `new_from`.
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
        context: nearest_function_context(input, first.before.start),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn algorithm_names_round_trip() {
        for algo in DiffAlgorithm::ALL {
            assert_eq!(DiffAlgorithm::parse(&algo.as_str()), Some(algo));
        }
        assert_eq!(DiffAlgorithm::parse("bogus"), None);
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
        // one unchanged line between two edits, context 3: 1 < 2*3, so they merge
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

    // hunk_context fixtures below are checked against real `git diff
    // --unified=0` output (see the finding this fixes), not guessed.

    #[test]
    fn hunk_context_finds_the_enclosing_function() {
        let old = "def parse_config():\n    a = 1\n    b = 2\n    c = 3\n";
        let new = "def parse_config():\n    a = 1\n    b = 20\n    c = 3\n";
        let hunks = histogram_hunks(old, new, "f.py", 0, true);
        assert_eq!(hunks[0].context, "def parse_config():");
    }

    #[test]
    fn hunk_context_skips_a_same_indent_sibling() {
        // `a = 1` sits right above the change at the same indentation as `b = 2`
        // and starts with a letter too, but git skips it for the def above it.
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
}
