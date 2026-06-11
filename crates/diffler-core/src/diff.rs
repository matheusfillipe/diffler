//! Intra-line diff computation: character-level change spans between a pair
//! of lines, used for word/char emphasis on top of line-level diffs.

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

/// A contiguous span of one side of a paired line diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub emphasized: bool,
}

/// Character-level diff of a paired old/new line.
///
/// Returns the spans for the old side and the new side. Unchanged regions are
/// not emphasized; inserted/deleted regions are.
///
/// ```
/// use diffler_core::diff::intraline;
///
/// let (old, new) = intraline("if x < y:", "if x <= y:");
/// assert!(new.iter().any(|s| s.emphasized));
/// assert_eq!(old.iter().map(|s| s.text.as_str()).collect::<String>(), "if x < y:");
/// ```
pub fn intraline(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    // graphemes, not chars: emphasis must never split a combining sequence
    // or emoji cluster, or the TUI styles half a glyph
    let diff = TextDiff::from_graphemes(old, new);
    let mut old_spans: Vec<Span> = Vec::new();
    let mut new_spans: Vec<Span> = Vec::new();

    for change in diff.iter_all_changes() {
        let (target, emphasized) = match change.tag() {
            ChangeTag::Equal => (None, false),
            ChangeTag::Delete => (Some(&mut old_spans), true),
            ChangeTag::Insert => (Some(&mut new_spans), true),
        };
        let value = change.value();
        if let Some(spans) = target {
            push_span(spans, value, emphasized);
        } else {
            push_span(&mut old_spans, value, false);
            push_span(&mut new_spans, value, false);
        }
    }

    (old_spans, new_spans)
}

fn push_span(spans: &mut Vec<Span>, text: &str, emphasized: bool) {
    if let Some(last) = spans.last_mut()
        && last.emphasized == emphasized
    {
        last.text.push_str(text);
        return;
    }
    spans.push(Span {
        text: text.to_owned(),
        emphasized,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn equal_lines_have_no_emphasis() {
        let (old, new) = intraline("same line", "same line");
        assert!(old.iter().all(|s| !s.emphasized));
        assert!(new.iter().all(|s| !s.emphasized));
        assert_eq!(joined(&old), "same line");
        assert_eq!(joined(&new), "same line");
    }

    #[test]
    fn spans_reconstruct_inputs() {
        let (old, new) = intraline(
            "if claims.expiry < now():",
            "if claims.expiry <= now() - LEEWAY:",
        );
        assert_eq!(joined(&old), "if claims.expiry < now():");
        assert_eq!(joined(&new), "if claims.expiry <= now() - LEEWAY:");
    }

    #[test]
    fn insertion_is_emphasized_on_new_side_only() {
        let (old, new) = intraline("session.touch()", "session.touch(now())");
        assert!(old.iter().all(|s| !s.emphasized));
        let emphasized: String = new
            .iter()
            .filter(|s| s.emphasized)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(emphasized, "now()");
    }

    #[test]
    fn adjacent_same_kind_spans_are_merged() {
        let (_, new) = intraline("ab", "aXYb");
        assert_eq!(
            new,
            vec![
                Span {
                    text: "a".into(),
                    emphasized: false
                },
                Span {
                    text: "XY".into(),
                    emphasized: true
                },
                Span {
                    text: "b".into(),
                    emphasized: false
                },
            ]
        );
    }

    #[test]
    fn combining_characters_stay_whole() {
        // "e\u{301}" is one grapheme; emphasis must cover it atomically
        let (_, new) = intraline("cafe", "cafe\u{301}");
        let emphasized: String = new
            .iter()
            .filter(|s| s.emphasized)
            .map(|s| s.text.as_str())
            .collect();
        assert!(emphasized.contains('\u{301}'));
        for span in &new {
            assert!(!span.text.starts_with('\u{301}'), "span splits a grapheme");
        }
    }

    #[test]
    fn empty_inputs() {
        let (old, new) = intraline("", "");
        assert!(old.is_empty());
        assert!(new.is_empty());
    }
}
