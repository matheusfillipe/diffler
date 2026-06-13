//! Pair deleted/added line runs inside a hunk and attach intra-line
//! emphasis. Lines pair positionally within a run, gated by similarity,
//! mirroring delta's homologous-line model.

use similar::TextDiff;

use crate::diff::intraline;
use crate::model::{DiffLine, FileDiff, Hunk, LineKind};

/// Below this similarity the pair is treated as unrelated (no emphasis).
const MIN_SIMILARITY: f32 = 0.4;

/// Attach intra-line emphasis to one file's hunks. Pairing is a render-time
/// concern (only the TUI reads `.emphasis`), so callers enrich the file they
/// are about to display rather than enriching whole models up front.
pub fn enrich_file(file: &mut FileDiff) {
    for hunk in &mut file.hunks {
        enrich_hunk(hunk);
    }
}

fn enrich_hunk(hunk: &mut Hunk) {
    let mut i = 0;
    while i < hunk.lines.len() {
        if kind_at(&hunk.lines, i) != Some(LineKind::Deleted) {
            i += 1;
            continue;
        }
        let del_start = i;
        while kind_at(&hunk.lines, i) == Some(LineKind::Deleted) {
            i += 1;
        }
        let add_start = i;
        while kind_at(&hunk.lines, i) == Some(LineKind::Added) {
            i += 1;
        }
        let pairs = (add_start - del_start).min(i - add_start);
        for p in 0..pairs {
            let (del_idx, add_idx) = (del_start + p, add_start + p);
            let (Some(old), Some(new)) = (hunk.lines.get(del_idx), hunk.lines.get(add_idx)) else {
                continue;
            };
            if similarity(&old.text, &new.text) < MIN_SIMILARITY {
                continue;
            }
            let (old_emphasis, new_emphasis) = intraline(&old.text, &new.text);
            if let Some(line) = hunk.lines.get_mut(del_idx) {
                line.emphasis = old_emphasis;
            }
            if let Some(line) = hunk.lines.get_mut(add_idx) {
                line.emphasis = new_emphasis;
            }
        }
    }
}

fn kind_at(lines: &[DiffLine], i: usize) -> Option<LineKind> {
    lines.get(i).map(|l| l.kind)
}

fn similarity(old: &str, new: &str) -> f32 {
    if old.is_empty() && new.is_empty() {
        return 1.0;
    }
    TextDiff::from_graphemes(old, new).ratio()
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
