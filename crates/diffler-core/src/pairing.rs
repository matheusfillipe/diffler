//! Pair deleted/added line runs inside a hunk and attach intra-line
//! emphasis. Lines pair positionally within a run, mirroring delta's
//! homologous-line model; the word-level engine's own token-similarity
//! floor rejects unrelated pairs.

use crate::diff::intraline;
use crate::model::{DiffLine, FileDiff, Hunk, LineKind};

/// Emphasis above this share of a line's content is noise, not signal:
/// highlights are for punctual edits, not rewrites. Word-level emphasis
/// legitimately covers whole tokens (`old_name` → `new_name` is most of its
/// line), so the ceiling sits above one-substituted-word territory; true
/// rewrites already fall out at the token-ratio gate.
const MAX_EMPHASIS_SHARE: f32 = 0.7;

/// More separate emphasis runs than this and the line reads as confetti:
/// scattered small edits render better as plain +/- lines (jj draws the
/// same line at 3 inline alternations). Counted after near-adjacent runs
/// merge under `diff::MAX_GAP_CHARS` — tune the two together.
const MAX_EMPHASIS_RUNS: usize = 3;

/// True when the emphasized ranges cover a minority of the line's
/// non-whitespace content in a few contiguous runs — a punctual edit worth
/// highlighting. A line that changed (nearly) everywhere, or in many
/// scattered places, reads better as a plain +/- line.
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
    // a couple of changed characters is always signal, whatever the ratio —
    // short lines ("41" → "42") would otherwise lose their only highlight;
    // the run cap stands regardless (whitespace-only runs count zero chars
    // and would ride the shortcut into confetti)
    content > 0
        && ranges.len() <= MAX_EMPHASIS_RUNS
        && (emphasized <= 2 || share(emphasized) < share(content) * MAX_EMPHASIS_SHARE)
}

/// Intra-line emphasis for a paired old/new line, gated as a pair: both
/// sides punctual, or neither side gets any.
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

/// Attach intra-line emphasis to one file's hunks. Pairing is a render-time
/// concern (only the TUI reads `.emphasis`), so callers enrich the file they
/// are about to display rather than enriching whole models up front.
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

/// `(deleted, added)` index pairs for a hunk's del/add runs, paired
/// positionally within each run — the shared homologous-line model.
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
        for p in 0..(add_start - del_start).min(i - add_start) {
            pairs.push((del_start + p, add_start + p));
        }
    }
    pairs
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
        assert!(h.lines[1].emphasis.is_empty()); // deletion side: nothing removed, only insert
        assert_eq!(h.lines[2].emphasis, vec![10..11]);
    }

    #[test]
    fn emphasis_is_punctual_separates_edits_from_rewrites() {
        // minority coverage is signal
        assert!(emphasis_is_punctual(
            "let x = compute();",
            std::slice::from_ref(&(8..15))
        ));
        // a tiny edit always qualifies, whatever the ratio
        assert!(emphasis_is_punctual("41", std::slice::from_ref(&(1..2))));
        // majority coverage is a rewrite: no char highlights
        assert!(!emphasis_is_punctual(
            "let x = compute();",
            std::slice::from_ref(&(0..14))
        ));
        assert!(!emphasis_is_punctual("", &[]));
    }

    #[test]
    fn scattered_runs_beyond_the_cap_are_not_punctual() {
        let text = "alpha one beta two gamma three delta four epsilon";
        // three runs under the coverage ceiling: still an edit
        let three = vec![6..9, 15..18, 25..30];
        assert!(emphasis_is_punctual(text, &three));
        // a fourth scattered run tips it into confetti
        let four = vec![6..9, 15..18, 25..30, 37..41];
        assert!(!emphasis_is_punctual(text, &four));
    }

    #[test]
    fn whitespace_only_runs_do_not_ride_the_tiny_edit_shortcut() {
        // alignment-only edits emphasize zero content chars; four scattered
        // space runs must still fail the cap, not pass as a "tiny edit"
        let text = "a   = 1; b   = 2; c   = 3; d   = 4";
        let runs = vec![1..4, 10..13, 19..22, 28..31];
        assert!(!emphasis_is_punctual(text, &runs));
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
    fn unbalanced_runs_pair_prefix_only() {
        let mut h = hunk(vec![
            (LineKind::Deleted, "alpha line one"),
            (LineKind::Deleted, "beta line two"),
            (LineKind::Added, "alpha line ONE"),
        ]);
        enrich_hunk(&mut h);
        assert!(!h.lines[2].emphasis.is_empty()); // paired with first deletion
        assert!(h.lines[1].emphasis.is_empty()); // unpaired deletion untouched
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
