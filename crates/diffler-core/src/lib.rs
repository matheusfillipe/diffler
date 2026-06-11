//! Core review engine for diffler: sessions, diff computation, verdicts, tasks.
//!
//! This crate holds all logic with no terminal dependency, so it can be tested
//! headless and reused by the TUI, the MCP server, and future frontends.

pub mod diff;
pub mod repo;
