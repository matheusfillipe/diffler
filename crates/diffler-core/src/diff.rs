//! Intra-line diff: byte ranges of changed regions between a paired
//! old/new line, used for char-level emphasis on top of line diffs.

use std::ops::Range;

use similar::{ChangeTag, TextDiff};

/// Byte ranges (into each input) that differ between the two lines.
///
/// Returns `(old_emphasis, new_emphasis)`. Adjacent ranges are merged.
///
/// ```
/// use diffler_core::diff::intraline;
///
/// let (old, new) = intraline("if x < y:", "if x <= y:");
/// assert!(old.is_empty());
/// assert_eq!(new, vec![6..7]);
/// ```
pub fn intraline(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    // graphemes, not chars: emphasis must never split a combining sequence
    // or emoji cluster, or the TUI styles half a glyph
    let diff = TextDiff::from_graphemes(old, new);
    let mut old_ranges: Vec<Range<usize>> = Vec::new();
    let mut new_ranges: Vec<Range<usize>> = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;

    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                old_pos += len;
                new_pos += len;
            }
            ChangeTag::Delete => {
                push_range(&mut old_ranges, old_pos..old_pos + len);
                old_pos += len;
            }
            ChangeTag::Insert => {
                push_range(&mut new_ranges, new_pos..new_pos + len);
                new_pos += len;
            }
        }
    }

    (old_ranges, new_ranges)
}

fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if let Some(last) = ranges.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    ranges.push(range);
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
    fn adjacent_ranges_are_merged() {
        let (_, new) = intraline("ab", "aXYb");
        assert_eq!(new, vec![1..3]);
    }

    #[test]
    fn combining_characters_stay_whole() {
        // "e\u{301}" is one grapheme; emphasis must cover it atomically
        let new_line = "cafe\u{301}";
        let (_, new) = intraline("cafe", new_line);
        for r in &new {
            assert!(new_line.is_char_boundary(r.start), "range splits a char");
            assert!(new_line.is_char_boundary(r.end), "range splits a char");
        }
        let joined: String = new.iter().map(|r| &new_line[r.clone()]).collect();
        assert!(joined.contains('\u{301}'));
    }

    #[test]
    fn empty_inputs() {
        let (old, new) = intraline("", "");
        assert!(old.is_empty());
        assert!(new.is_empty());
    }
}
