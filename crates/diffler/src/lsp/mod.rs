//! Language-server access for the blast-radius view: which callers a changed
//! symbol has beyond the diff. One client per language, spawned on demand.

mod client;
mod registry;

pub use client::LspClient;
pub use registry::{Resolution, ServerSpec, candidates, resolve};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LspError {
    #[error("cannot start {0}: {1}")]
    Spawn(String, String),
    #[error("lsp transport: {0}")]
    Io(&'static str),
    #[error("{0} failed: {1}")]
    Server(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub select_line: u32,
    pub select_character: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSite {
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub name: String,
    pub path: String,
    pub line: u32,
    pub select_line: u32,
    pub select_character: u32,
}
