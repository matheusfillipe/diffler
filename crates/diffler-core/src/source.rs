//! What a review is of, with a filesystem-safe persistence key and a label.

use serde::{Deserialize, Serialize};

/// Characters of an oid shown in a label; full oids stay in the key.
const SHORT_OID: usize = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewSource {
    #[default]
    WorkingTree,
    Commit {
        oid: String,
    },
    Range {
        oldest: String,
        newest: String,
    },
    /// Keyed on the number so review state survives pushes.
    Pr {
        number: u64,
    },
    /// The working tree three-dot against `rev`. We store `rev` as named and
    /// resolve it at diff time, so the review follows the ref.
    Against {
        rev: String,
    },
    Walkthrough {
        id: String,
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

    pub fn walkthrough(id: impl Into<String>) -> Self {
        Self::Walkthrough { id: id.into() }
    }

    /// Persistence key and filename stem. Oids are dash-free hex, so `-` is an
    /// unambiguous separator; refs and walkthrough ids are sanitised.
    pub fn key(&self) -> String {
        match self {
            Self::WorkingTree => "working".to_owned(),
            Self::Commit { oid } => format!("commit-{oid}"),
            Self::Range { oldest, newest } => format!("range-{oldest}-{newest}"),
            Self::Pr { number } => format!("pr-{number}"),
            Self::Against { rev } => format!("against-{}", filename_safe(rev)),
            Self::Walkthrough { id } => format!("walkthrough-{}", filename_safe(id)),
        }
    }

    /// A walkthrough's title lives in its session, so it labels by id here.
    pub fn label(&self) -> String {
        match self {
            Self::WorkingTree => "working tree".to_owned(),
            Self::Commit { oid } => format!("commit {}", short(oid)),
            Self::Range { oldest, newest } => {
                format!("range {}..{}", short(oldest), short(newest))
            }
            Self::Pr { number } => format!("PR #{number}"),
            Self::Against { rev } => format!("vs {}", short_rev(rev)),
            Self::Walkthrough { id } => format!("walkthrough {}", short_id(id)),
        }
    }
}

fn short(oid: &str) -> &str {
    oid.get(..SHORT_OID).unwrap_or(oid)
}

const SHORT_ID: usize = 8;

fn short_id(id: &str) -> &str {
    id.get(..SHORT_ID).unwrap_or(id)
}

/// A raw oid shortens like the other arms; a ref name stays whole.
fn short_rev(rev: &str) -> &str {
    if rev.len() >= SHORT_OID && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        short(rev)
    } else {
        rev
    }
}

/// Collapses unsafe characters to `-`, so `feat/x` and `feat-x` share a file.
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
        assert_eq!(ReviewSource::walkthrough("w1").key(), "walkthrough-w1");
    }

    #[test]
    fn against_keys_are_filename_safe() {
        assert_eq!(
            ReviewSource::against("origin/main").key(),
            "against-origin-main"
        );
        assert_eq!(ReviewSource::against("HEAD~1").key(), "against-HEAD-1");
        assert_eq!(
            ReviewSource::against("feat/x").key(),
            ReviewSource::against("feat-x").key()
        );
    }

    #[test]
    fn walkthrough_keys_are_filename_safe() {
        assert_eq!(
            ReviewSource::walkthrough("feature/login").key(),
            "walkthrough-feature-login"
        );
        assert_eq!(
            ReviewSource::walkthrough("../../etc/passwd").key(),
            "walkthrough-..-..-etc-passwd"
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
        assert_eq!(
            ReviewSource::walkthrough("0123456789abcdef").label(),
            "walkthrough 01234567"
        );
    }

    #[test]
    fn short_oid_tolerates_a_short_string() {
        assert_eq!(ReviewSource::commit("ab").label(), "commit ab");
        assert_eq!(ReviewSource::walkthrough("ab").label(), "walkthrough ab");
    }

    #[test]
    fn round_trips_through_json_as_a_tagged_descriptor() {
        for source in [
            ReviewSource::WorkingTree,
            ReviewSource::commit("abc"),
            ReviewSource::range("aaa", "bbb"),
            ReviewSource::pr(3),
            ReviewSource::against("origin/main"),
            ReviewSource::walkthrough("w1"),
        ] {
            let json = serde_json::to_string(&source).expect("serialize");
            let back: ReviewSource = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(source, back);
        }
    }
}
