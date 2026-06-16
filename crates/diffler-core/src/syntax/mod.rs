//! Tree-sitter foundation shared by highlighting, scope context, and the
//! structural diff. Grammars are statically linked (no runtime loading, since
//! musl-static binaries cannot `dlopen`); a parse failure or unknown language
//! degrades silently to plain behavior so the UI is never blocked.

pub mod registry;

pub use registry::{HIGHLIGHT_NAMES, LangEntry, LanguageRegistry};
