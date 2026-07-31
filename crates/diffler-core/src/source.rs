//! What a review is *of*: the working tree, a single commit, a contiguous
//! commit range, or everything since a named revision. A source has a
//! deterministic, filesystem-safe persistence key and a human-facing label, so
//! review state can be tracked per source and the agent can be told exactly
//! what the human reviewed.

use serde::{Deserialize, Serialize};

/// Characters of an oid shown in a label; full oids stay in the key.
const SHORT_OID: usize = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewSource {
    WorkingTree,
    Commit {
        oid: String,
    },
    Range {
        oldest: String,
        newest: String,
    },
    /// A forge pull request. The diff renders as a range resolved at open
    /// time, but review state keys on the PR number so it survives pushes.
    Pr {
        number: u64,
    },
    /// Everything the working tree carries over `rev`, three-dot. `rev` is
    /// stored as the human named it and resolved at diff time, so the review
    /// follows the ref as it moves.
    Against {
        rev: String,
    },
}

impl ReviewSource {
    pub fn commit(oid: impl Into<String>) -> Self {
        Self::Commit { oid: oid.into() }
    }

    pub fn range(oldest: impl Into<String>, newest: impl Into<String>) -> Self {
        Self::Range {
            oldest: oldest.into(),
            newest: newest.into(),
        }
    }

    pub fn pr(number: u64) -> Self {
        Self::Pr { number }
    }

    pub fn against(rev: impl Into<String>) -> Self {
        Self::Against { rev: rev.into() }
    }

    /// Stable persistence key, also the on-disk filename stem. The `-`
    /// separator is unambiguous because git/jj oids are dash-free hex; every
    /// character is filesystem-safe.
    pub fn key(&self) -> String {
        match self {
            Self::WorkingTree => "working".to_owned(),
            Self::Commit { oid } => format!("commit-{oid}"),
            Self::Range { oldest, newest } => format!("range-{oldest}-{newest}"),
            Self::Pr { number } => format!("pr-{number}"),
            Self::Against { rev } => format!("against-{}", filename_safe(rev)),
        }
    }

    /// Human-facing description of what is being reviewed.
    pub fn label(&self) -> String {
        match self {
            Self::WorkingTree => "working tree".to_owned(),
            Self::Commit { oid } => format!("commit {}", short(oid)),
            Self::Range { oldest, newest } => {
                format!("range {}..{}", short(oldest), short(newest))
            }
            Self::Pr { number } => format!("PR #{number}"),
            Self::Against { rev } => format!("vs {}", short_rev(rev)),
        }
    }
}

fn short(oid: &str) -> &str {
    oid.get(..SHORT_OID).unwrap_or(oid)
}

/// A raw oid shortens like the other arms; a ref name stays whole.
fn short_rev(rev: &str) -> &str {
    if rev.len() >= SHORT_OID && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        short(rev)
    } else {
        rev
    }
}

/// Ref names carry `/` and other characters a filename cannot, so they collapse
/// to `-`. `feat/x` and `feat-x` therefore share one review file, the accepted
/// cost of a flat key.
fn filename_safe(rev: &str) -> String {
    rev.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_deterministic_and_distinct_per_source() {
        assert_eq!(ReviewSource::WorkingTree.key(), "working");
        assert_eq!(ReviewSource::commit("abc123").key(), "commit-abc123");
        assert_eq!(ReviewSource::range("aaa", "bbb").key(), "range-aaa-bbb");
        assert_eq!(ReviewSource::pr(42).key(), "pr-42");
        assert_eq!(ReviewSource::against("main").key(), "against-main");
    }

    #[test]
    fn against_keys_are_filename_safe() {
        assert_eq!(
            ReviewSource::against("origin/main").key(),
            "against-origin-main"
        );
        assert_eq!(ReviewSource::against("HEAD~1").key(), "against-HEAD-1");
        // the documented collision: a flat key cannot tell these apart
        assert_eq!(
            ReviewSource::against("feat/x").key(),
            ReviewSource::against("feat-x").key()
        );
    }

    #[test]
    fn labels_shorten_oids() {
        assert_eq!(ReviewSource::WorkingTree.label(), "working tree");
        assert_eq!(
            ReviewSource::commit("0123456789abcdef").label(),
            "commit 0123456"
        );
        assert_eq!(
            ReviewSource::range("0123456789", "fedcba9876").label(),
            "range 0123456..fedcba9"
        );
        assert_eq!(ReviewSource::against("main").label(), "vs main");
        assert_eq!(
            ReviewSource::against("origin/main").label(),
            "vs origin/main"
        );
        assert_eq!(ReviewSource::against("HEAD~1").label(), "vs HEAD~1");
        assert_eq!(
            ReviewSource::against("0123456789abcdef").label(),
            "vs 0123456"
        );
    }

    #[test]
    fn short_oid_tolerates_a_short_string() {
        assert_eq!(ReviewSource::commit("ab").label(), "commit ab");
    }

    #[test]
    fn round_trips_through_json_as_a_tagged_descriptor() {
        for source in [
            ReviewSource::WorkingTree,
            ReviewSource::commit("abc"),
            ReviewSource::range("aaa", "bbb"),
            ReviewSource::pr(3),
            ReviewSource::against("origin/main"),
        ] {
            let json = serde_json::to_string(&source).expect("serialize");
            let back: ReviewSource = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(source, back);
        }
    }
}
