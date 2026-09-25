//! Selectable line-diff algorithms, shared by every diff source. `Myers`,
//! `Minimal` and `Patience` are git2's own (see [`crate::git`]); `Histogram`
//! and `Structural` (which layers reformat detection on top, in
//! [`crate::syntax::intraline`]) run through imara-diff, since libgit2 has no
//! histogram implementation.

use std::collections::HashMap;

use imara_diff::{Algorithm, Diff, InternedInput};
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
    /// The histogram line diff, plus: a paired deleted/added line whose
    /// tokens are structurally identical (a pure reformat) renders as one
    /// dimmed line instead of red/green.
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

    /// Whether this algorithm runs through imara-diff instead of git2, since
    /// libgit2 has no histogram implementation.
    pub const fn is_imara(self) -> bool {
        matches!(self, Self::Histogram | Self::Structural)
    }
}

impl std::fmt::Display for DiffAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
    let id = disambiguated_hunk_id(file_path, &lines, seen);
    Hunk {
        id,
        old_start: lead_start + 1,
        old_lines,
        new_start: after_lead_start + 1,
        new_lines,
        context: String::new(),
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
    fn hunk_ids_are_stable_for_identical_input() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let first = histogram_hunks(old, new, "f.txt", 1, true);
        let second = histogram_hunks(old, new, "f.txt", 1, true);
        assert_eq!(first[0].id, second[0].id);
    }
}
