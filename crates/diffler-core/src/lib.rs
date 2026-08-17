//! Core review engine for diffler: diff computation, sessions, comments, viewed marks.
//!
//! This crate holds all logic with no terminal dependency, so it can be tested
//! headless and reused by the TUI, the MCP server, and future frontends.

pub mod classify;
pub mod diff;
pub mod feedback;
pub mod git;
pub mod highlight;
pub mod language;
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
