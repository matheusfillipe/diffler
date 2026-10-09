//! Core review engine for diffler, with no terminal dependency: diffs,
//! sessions, comments, viewed marks.

pub mod classify;
pub mod diff;
pub mod diffalgo;
pub mod feedback;
pub mod git;
pub mod highlight;
pub mod jj;
pub mod language;
pub mod lens;
pub mod model;
pub mod pairing;
pub mod repo;
pub mod review;
pub mod session;
pub mod source;
pub mod stats;
pub mod store;
pub mod syntax;
#[cfg(feature = "test-support")]
pub mod test_git;
#[cfg(test)]
pub(crate) mod test_support;
pub mod vcs;
pub mod walkthrough;
