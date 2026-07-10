//! Intra-line diff: byte ranges of changed regions between a paired
//! old/new line, used for word-level emphasis on top of line diffs.

use std::ops::Range;

use similar::{ChangeTag, InlineChangeMode, InlineChangeOptions, TextDiff};

/// Below this token-level similarity the pair reads better as plain +/-
/// lines; the refinement falls back to unemphasized output under it.
const MIN_INLINE_RATIO: f32 = 0.5;

/// Emphasis runs separated by this many characters or fewer merge into one
/// span: two highlights straddling a two-char gap read as noise, one reads
/// as the edit. Merging happens before `pairing::MAX_EMPHASIS_RUNS` counts
/// the runs — tune the two together.
const MAX_GAP_CHARS: usize = 2;

/// Byte ranges (into each input) that differ between the two lines.
///
/// Word-level, not char-level: lines tokenize into unicode words,
/// punctuation, and whitespace (UAX #29), so unrelated tokens can never
/// share a stray letter (`npm` → `bun` is a whole-token swap, not an edit
/// around a common `n`). A semantic-cleanup pass absorbs coincidental
/// matches and snaps boundaries to word edges, mirroring what GitHub-class
/// diff viewers ship.
///
/// Returns `(old_emphasis, new_emphasis)`. Adjacent and near-adjacent
/// ranges are merged.
///
/// ```
/// use diffler_core::diff::intraline;
///
/// let (old, new) = intraline("if x < y:", "if x <= y:");
/// assert!(old.is_empty());
/// assert_eq!(new, vec![6..7]);
/// ```
pub fn intraline(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let diff = TextDiff::from_lines(old, new);
    let mut options = InlineChangeOptions::new();
    options
        .mode(InlineChangeMode::UnicodeWords)
        .semantic_cleanup(true)
        .min_ratio(MIN_INLINE_RATIO);

    // positions accumulate globally per side: `from_lines` splits on embedded
    // `\r` too, and a fresh counter per segment would emit segment-local
    // offsets where the caller expects offsets into the whole line
    let mut old_ranges: Vec<Range<usize>> = Vec::new();
    let mut new_ranges: Vec<Range<usize>> = Vec::new();
    let (mut old_pos, mut new_pos) = (0usize, 0usize);
    for op in diff.ops() {
        for change in diff.iter_inline_changes_with_options(op, options) {
            let piece_len: usize = change.values().iter().map(|(_, piece)| piece.len()).sum();
            let (ranges, pos) = match change.tag() {
                ChangeTag::Delete => (&mut old_ranges, &mut old_pos),
                ChangeTag::Insert => (&mut new_ranges, &mut new_pos),
                ChangeTag::Equal => {
                    old_pos += piece_len;
                    new_pos += piece_len;
                    continue;
                }
            };
            for &(emphasized, piece) in change.values() {
                if emphasized {
                    ranges.push(*pos..*pos + piece.len());
                }
                *pos += piece.len();
            }
        }
    }

    (coalesce(old, old_ranges), coalesce(new, new_ranges))
}

/// Merge runs whose gap is `MAX_GAP_CHARS` characters or fewer.
fn coalesce(text: &str, ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = out.last_mut()
            && text
                .get(last.end..range.start)
                .is_some_and(|gap| gap.chars().count() <= MAX_GAP_CHARS)
        {
            last.end = range.end;
            continue;
        }
        out.push(range);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_lines_have_no_emphasis() {
        let (old, new) = intraline("same line", "same line");
        assert!(old.is_empty());
        assert!(new.is_empty());
    }

    #[test]
    fn ranges_are_in_bounds_and_ascending() {
        let old = "if claims.expiry < now():";
        let new = "if claims.expiry <= now() - LEEWAY:";
        let (old_r, new_r) = intraline(old, new);
        for r in &old_r {
            assert!(r.end <= old.len());
        }
        let mut prev_end = 0;
        for r in &new_r {
            assert!(r.start >= prev_end && r.end <= new.len());
            prev_end = r.end;
        }
    }

    #[test]
    fn insertion_is_emphasized_on_new_side_only() {
        let (old, new) = intraline("session.touch()", "session.touch(now())");
        assert!(old.is_empty());
        let joined: String = new
            .iter()
            .map(|r| &"session.touch(now())"[r.clone()])
            .collect();
        assert_eq!(joined, "now()");
    }

    #[test]
    fn changed_word_is_emphasized_whole_never_fragmented() {
        // char-level LCS latches onto the shared `n` of npm/bun; word-level
        // must swap the whole token
        let old = "npm run lint";
        let new = "bun run lint";
        let (old_r, new_r) = intraline(old, new);
        assert_eq!(old_r, vec![0..3]);
        assert_eq!(new_r, vec![0..3]);
    }

    #[test]
    fn prose_edit_emphasizes_only_the_changed_words() {
        let old = "runs `better-auth migrate` against src";
        let new = "runs `auth migrate` against src";
        let (old_r, new_r) = intraline(old, new);
        let joined: String = old_r.iter().map(|r| &old[r.clone()]).collect();
        assert_eq!(joined, "better-");
        assert!(new_r.is_empty(), "new side only lost words: {new_r:?}");
    }

    #[test]
    fn dissimilar_lines_fall_back_to_no_emphasis() {
        // token overlap below the ratio floor: whole-line rewrite, no confetti
        let (old, new) = intraline(
            "npm run migrate -w services/auth",
            "bun run --filter '@syte-tech/auth-service' migrate",
        );
        assert!(old.is_empty(), "{old:?}");
        assert!(new.is_empty(), "{new:?}");
    }

    #[test]
    fn tiny_gaps_between_runs_merge_into_one_span() {
        let text = "ab";
        let merged = coalesce(text, vec![0..1, 1..2]);
        assert_eq!(merged, vec![0..2]);
        let text = "a--b";
        let merged = coalesce(text, vec![0..1, 3..4]);
        assert_eq!(merged, vec![0..4]);
        let text = "a---b";
        let merged = coalesce(text, vec![0..1, 4..5]);
        assert_eq!(merged, vec![0..1, 4..5]);
    }

    #[test]
    fn combining_characters_stay_whole() {
        // "e\u{301}" is one grapheme inside a word token; emphasis must
        // cover it atomically
        let old_line = "drink cafe daily";
        let new_line = "drink cafe\u{301} daily";
        let (_, new) = intraline(old_line, new_line);
        for r in &new {
            assert!(new_line.is_char_boundary(r.start), "range splits a char");
            assert!(new_line.is_char_boundary(r.end), "range splits a char");
        }
        let joined: String = new.iter().map(|r| &new_line[r.clone()]).collect();
        assert!(joined.contains('\u{301}'), "emphasis: {new:?}");
    }

    #[test]
    fn empty_inputs() {
        let (old, new) = intraline("", "");
        assert!(old.is_empty());
        assert!(new.is_empty());
    }

    #[test]
    fn embedded_carriage_returns_keep_offsets_global() {
        // a lone `\r` splits the text into segments internally; emphasis
        // offsets must still address the whole line
        let old = "alpha\rfoo bar baz";
        let new = "alpha\rfoo QUX baz";
        let (old_r, new_r) = intraline(old, new);
        let covered: String = old_r.iter().map(|r| &old[r.clone()]).collect();
        assert_eq!(covered, "bar", "{old_r:?}");
        let covered: String = new_r.iter().map(|r| &new[r.clone()]).collect();
        assert_eq!(covered, "QUX", "{new_r:?}");

        // multibyte text before the `\r` must not desync byte offsets
        let old = "héé\rfoo bar baz";
        let new = "héé\rfoo QUX baz";
        let (old_r, new_r) = intraline(old, new);
        for r in old_r.iter().chain(&new_r) {
            assert!(old.is_char_boundary(r.start) && old.is_char_boundary(r.end));
        }
        let covered: String = new_r.iter().map(|r| &new[r.clone()]).collect();
        assert_eq!(covered, "QUX", "{new_r:?}");

        // edits in two segments emphasize each in place, in order
        let old = "aa bb cc\rdd ee ff";
        let new = "aa XX cc\rdd YY ff";
        let (_, new_r) = intraline(old, new);
        let covered: Vec<&str> = new_r.iter().map(|r| &new[r.clone()]).collect();
        assert_eq!(covered, ["XX", "YY"], "{new_r:?}");
    }
}
