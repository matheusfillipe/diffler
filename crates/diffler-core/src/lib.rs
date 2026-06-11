//! Core review engine for diffler: diff computation, sessions, comments, viewed marks.
//!
//! This crate holds all logic with no terminal dependency, so it can be tested
//! headless and reused by the TUI, the MCP server, and future frontends.

pub mod diff;
pub mod model;
pub mod repo;
