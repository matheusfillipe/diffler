//! What a review is *of*: the working tree, a single commit, or a contiguous
//! commit range. A source has a deterministic, filesystem-safe persistence key
//! and a human-facing label, so review state can be tracked per source and the
//! agent can be told exactly what the human reviewed.

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

    /// Stable persistence key, also the on-disk filename stem. The `-`
    /// separator is unambiguous because git/jj oids are dash-free hex; every
    /// character is filesystem-safe.
    pub fn key(&self) -> String {
        match self {
            Self::WorkingTree => "working".to_owned(),
            Self::Commit { oid } => format!("commit-{oid}"),
            Self::Range { oldest, newest } => format!("range-{oldest}-{newest}"),
            Self::Pr { number } => format!("pr-{number}"),
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
        }
    }
}

fn short(oid: &str) -> &str {
    oid.get(..SHORT_OID).unwrap_or(oid)
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
        ] {
            let json = serde_json::to_string(&source).expect("serialize");
            let back: ReviewSource = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(source, back);
        }
    }
}
