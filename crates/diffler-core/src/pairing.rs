//! Pair deleted/added line runs by best total similarity (delta's
//! homologous-line model) and attach intra-line emphasis. Unpaired lines
//! render plain.

use similar::TextDiff;

use crate::diff::intraline;
use crate::model::{DiffLine, FileDiff, Hunk, LineKind};

/// Word-level emphasis covers whole tokens (`old_name` → `new_name` is most
/// of its line), so we set this above one substituted word.
const MAX_EMPHASIS_SHARE: f32 = 0.7;

/// Counted after near-adjacent runs merge under `diff::MAX_GAP_CHARS`, so we
/// tune the two together. jj uses the same cap.
const MAX_EMPHASIS_RUNS: usize = 3;

/// True when the emphasized ranges cover a minority of the line's
/// non-whitespace content in a few runs.
pub(crate) fn emphasis_is_punctual(text: &str, ranges: &[std::ops::Range<usize>]) -> bool {
    // usize→f32 precision loss is irrelevant at line lengths
    #[allow(clippy::cast_precision_loss)]
    fn share(n: usize) -> f32 {
        n as f32
    }
    let mut content = 0usize;
    let mut emphasized = 0usize;
    for (i, &b) in text.as_bytes().iter().enumerate() {
        if b == b' ' || b == b'\t' {
            continue;
        }
        content += 1;
        if ranges.iter().any(|r| r.start <= i && i < r.end) {
            emphasized += 1;
        }
    }
    // we always keep two changed characters so "41" → "42" stays lit, and
    // keep the run cap regardless since whitespace runs count zero chars
    content > 0
        && ranges.len() <= MAX_EMPHASIS_RUNS
        && (emphasized <= 2 || share(emphasized) < share(content) * MAX_EMPHASIS_SHARE)
}

/// Both sides punctual, or neither side gets emphasis.
pub(crate) fn gated_pair_emphasis(
    old: &str,
    new: &str,
) -> (Vec<std::ops::Range<usize>>, Vec<std::ops::Range<usize>>) {
    let (old_emphasis, new_emphasis) = intraline(old, new);
    if emphasis_is_punctual(old, &old_emphasis) && emphasis_is_punctual(new, &new_emphasis) {
        (old_emphasis, new_emphasis)
    } else {
        (Vec::new(), Vec::new())
    }
}

pub fn enrich_file(file: &mut FileDiff) {
    for hunk in &mut file.hunks {
        enrich_hunk(hunk);
    }
}

fn enrich_hunk(hunk: &mut Hunk) {
    for (del_idx, add_idx) in paired_run_indices(&hunk.lines) {
        let (Some(old), Some(new)) = (hunk.lines.get(del_idx), hunk.lines.get(add_idx)) else {
            continue;
        };
        let (old_emphasis, new_emphasis) = gated_pair_emphasis(&old.text, &new.text);
        if let Some(line) = hunk.lines.get_mut(del_idx) {
            line.emphasis = old_emphasis;
        }
        if let Some(line) = hunk.lines.get_mut(add_idx) {
            line.emphasis = new_emphasis;
        }
    }
}

/// Keep at or above `diff::MIN_INLINE_RATIO`, or pairs form whose emphasis
/// it always suppresses.
const MIN_PAIR_RATIO: f32 = 0.5;

/// A run past this is a rewrite, so we pair it by position and skip the
/// quadratic alignment.
const MAX_PAIR_TABLE: usize = 1024;

/// `(deleted, added)` index pairs for a hunk's del/add runs.
pub(crate) fn paired_run_indices(lines: &[DiffLine]) -> Vec<(usize, usize)> {
    let kind_at = |i: usize| lines.get(i).map(|l| l.kind);
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if kind_at(i) != Some(LineKind::Deleted) {
            i += 1;
            continue;
        }
        let del_start = i;
        while kind_at(i) == Some(LineKind::Deleted) {
            i += 1;
        }
        let add_start = i;
        while kind_at(i) == Some(LineKind::Added) {
            i += 1;
        }
        pair_runs(lines, del_start..add_start, add_start..i, &mut pairs);
    }
    pairs
}

/// Weighted LCS over line pairs. Positional pairing mismatches as soon as a
/// run inserts or drops one line.
// the DP tables are allocated (d+1)×(a+1) and every index below stays
// inside those bounds
#[allow(clippy::indexing_slicing)]
fn pair_runs(
    lines: &[DiffLine],
    dels: std::ops::Range<usize>,
    adds: std::ops::Range<usize>,
    pairs: &mut Vec<(usize, usize)>,
) {
    let (d, a) = (dels.len(), adds.len());
    if d == 0 || a == 0 {
        return;
    }
    if d * a > MAX_PAIR_TABLE {
        pairs.extend((0..d.min(a)).map(|p| (dels.start + p, adds.start + p)));
        return;
    }
    let text = |i: usize| lines.get(i).map_or("", |l| l.text.as_str());
    // score[i][j]: best total ratio pairing the first i dels with the first
    // j adds; step[i][j] records the move that produced it for traceback
    let mut score = vec![vec![0f32; a + 1]; d + 1];
    let mut step = vec![vec![0u8; a + 1]; d + 1];
    for i in 1..=d {
        for j in 1..=a {
            let ratio = line_ratio(text(dels.start + i - 1), text(adds.start + j - 1));
            let (mut best, mut chose) = (score[i - 1][j], 1u8);
            if score[i][j - 1] > best {
                (best, chose) = (score[i][j - 1], 2);
            }
            // >= so an exact tie prefers pairing (the positional alignment):
            // shifted single-token columns land exactly on the ratio floor
            if ratio >= MIN_PAIR_RATIO && score[i - 1][j - 1] + ratio >= best {
                (best, chose) = (score[i - 1][j - 1] + ratio, 3);
            }
            score[i][j] = best;
            step[i][j] = chose;
        }
    }
    let (mut i, mut j) = (d, a);
    let mut aligned = Vec::new();
    while i > 0 && j > 0 {
        match step[i][j] {
            3 => {
                aligned.push((dels.start + i - 1, adds.start + j - 1));
                i -= 1;
                j -= 1;
            }
            2 => j -= 1,
            _ => i -= 1,
        }
    }
    pairs.extend(aligned.into_iter().rev());
}

/// The token diff per DP cell is quadratic on dissimilar lines, so longer
/// lines never pair.
const MAX_PAIR_LINE_BYTES: usize = 1024;

/// Indentation counts, since a shared indent keeps short pairs like
/// `41` → `42` above the floor.
fn line_ratio(old: &str, new: &str) -> f32 {
    if old.len() > MAX_PAIR_LINE_BYTES || new.len() > MAX_PAIR_LINE_BYTES {
        return 0.0;
    }
    // we score blank lines zero so a stray blank cannot take a real
    // homolog's slot
    if old.trim().is_empty() || new.trim().is_empty() {
        return 0.0;
    }
    TextDiff::from_unicode_words(old, new).ratio()
}

#[cfg(test)]
mod tests {
    use crate::model::{DiffLine, HunkId, LineKind};

    use super::*;

    fn hunk(lines: Vec<(LineKind, &str)>) -> Hunk {
        Hunk {
            id: HunkId("test".into()),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            context: String::new(),
            lines: lines
                .into_iter()
                .map(|(k, t)| DiffLine::new(k, None, None, t.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn similar_pair_gets_emphasis_on_both_sides() {
        let mut h = hunk(vec![
            (LineKind::Context, "def f():"),
            (LineKind::Deleted, "    if x < y:"),
            (LineKind::Added, "    if x <= y:"),
        ]);
        enrich_hunk(&mut h);
        assert!(h.lines[1].emphasis.is_empty());
        assert_eq!(h.lines[2].emphasis, vec![10..11]);
    }

    #[test]
    fn emphasis_is_punctual_separates_edits_from_rewrites() {
        assert!(emphasis_is_punctual(
            "let x = compute();",
            std::slice::from_ref(&(8..15))
        ));
        assert!(emphasis_is_punctual("41", std::slice::from_ref(&(1..2))));
        assert!(!emphasis_is_punctual(
            "let x = compute();",
            std::slice::from_ref(&(0..14))
        ));
        assert!(!emphasis_is_punctual("", &[]));
    }

    #[test]
    fn scattered_runs_beyond_the_cap_are_not_punctual() {
        let text = "alpha one beta two gamma three delta four epsilon";
        let three = vec![6..9, 15..18, 25..30];
        assert!(emphasis_is_punctual(text, &three));
        let four = vec![6..9, 15..18, 25..30, 37..41];
        assert!(!emphasis_is_punctual(text, &four));
    }

    #[test]
    fn whitespace_only_runs_do_not_ride_the_tiny_edit_shortcut() {
        let text = "a   = 1; b   = 2; c   = 3; d   = 4";
        let runs = vec![1..4, 10..13, 19..22, 28..31];
        assert!(!emphasis_is_punctual(text, &runs));
    }

    #[test]
    fn shifted_single_token_columns_still_pair_positionally() {
        // "    1" vs "    2" sits exactly on the ratio floor
        let mut h = hunk(vec![
            (LineKind::Deleted, "    1"),
            (LineKind::Deleted, "    2"),
            (LineKind::Added, "    2"),
            (LineKind::Added, "    3"),
        ]);
        enrich_hunk(&mut h);
        assert_eq!(paired_run_indices(&h.lines), vec![(0, 2), (1, 3)]);
        assert_eq!(h.lines[0].emphasis, vec![4..5]);
        assert_eq!(h.lines[3].emphasis, vec![4..5]);
    }

    #[test]
    fn blank_lines_never_steal_a_homolog_slot() {
        let mut h = hunk(vec![
            (LineKind::Deleted, "foo();"),
            (LineKind::Deleted, ""),
            (LineKind::Added, ""),
            (LineKind::Added, "foo(x);"),
        ]);
        enrich_hunk(&mut h);
        assert_eq!(paired_run_indices(&h.lines), vec![(0, 3)]);
        assert!(!h.lines[3].emphasis.is_empty(), "the edit keeps emphasis");
    }

    #[test]
    fn huge_lines_never_pair() {
        let long_old = format!("data,{}", "x,".repeat(1024));
        let long_new = format!("data,{}", "y,".repeat(1024));
        let mut h = hunk(vec![
            (LineKind::Deleted, long_old.as_str()),
            (LineKind::Added, long_new.as_str()),
        ]);
        enrich_hunk(&mut h);
        assert!(paired_run_indices(&h.lines).is_empty());
        assert!(h.lines.iter().all(|l| l.emphasis.is_empty()));
    }

    #[test]
    fn dissimilar_pair_gets_no_emphasis() {
        let mut h = hunk(vec![
            (LineKind::Deleted, "totally_different_thing()"),
            (LineKind::Added, "x = 1"),
        ]);
        enrich_hunk(&mut h);
        assert!(h.lines[0].emphasis.is_empty());
        assert!(h.lines[1].emphasis.is_empty());
    }

    #[test]
    fn unbalanced_runs_pair_by_similarity_not_position() {
        let mut h = hunk(vec![
            (LineKind::Deleted, "alpha line one"),
            (LineKind::Deleted, "beta line two"),
            (LineKind::Added, "beta line TWO"),
        ]);
        enrich_hunk(&mut h);
        assert_eq!(
            paired_run_indices(&h.lines),
            vec![(1, 2)],
            "pairs the beta lines, leaves alpha unpaired"
        );
        assert!(!h.lines[2].emphasis.is_empty());
        assert!(h.lines[0].emphasis.is_empty());
    }

    #[test]
    fn misaligned_type_hunk_pairs_fields_with_their_homologs() {
        let mut h = hunk(vec![
            (
                LineKind::Deleted,
                "export function buildTokenClaims(user: {",
            ),
            (LineKind::Deleted, "    email: string;"),
            (LineKind::Deleted, "    permissions?: string[];"),
            (LineKind::Deleted, "    states?: string[];"),
            (LineKind::Added, "type Entitlements = {"),
            (LineKind::Added, "    permissions?: string[] | null;"),
            (LineKind::Added, "    states?: string[] | null;"),
        ]);
        enrich_hunk(&mut h);
        assert_eq!(paired_run_indices(&h.lines), vec![(2, 5), (3, 6)]);
        for (index, expected) in [(5, "| null"), (6, "| null")] {
            let line = &h.lines[index];
            let covered: String = line
                .emphasis
                .iter()
                .map(|r| &line.text[r.clone()])
                .collect();
            assert_eq!(
                covered.trim(),
                expected,
                "only the added union arm lights up: {covered:?}"
            );
        }
        for index in [0, 1, 4] {
            assert!(
                h.lines[index].emphasis.is_empty(),
                "unpaired line {index} renders plain"
            );
        }
    }

    #[test]
    fn separate_runs_pair_independently() {
        let mut h = hunk(vec![
            (LineKind::Deleted, "first old line"),
            (LineKind::Added, "first new line"),
            (LineKind::Context, "middle"),
            (LineKind::Deleted, "second old line"),
            (LineKind::Added, "second new line"),
        ]);
        enrich_hunk(&mut h);
        assert!(!h.lines[0].emphasis.is_empty());
        assert!(!h.lines[1].emphasis.is_empty());
        assert!(!h.lines[3].emphasis.is_empty());
        assert!(!h.lines[4].emphasis.is_empty());
    }
}
