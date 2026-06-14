//! diffler binary internals, exposed as a library so the integration tests
//! can drive the full app (event pump + MCP server) headlessly. The public
//! surface is the binary's: nothing here is a stable API.

pub mod app;
pub mod clipboard;
pub mod config;
pub mod editor;
pub mod event;
pub mod keymap;
pub mod mcp;
#[cfg(test)]
mod test_support;
pub mod theme;
pub mod transient;
pub mod tree;
pub mod ui;
pub mod watch;
