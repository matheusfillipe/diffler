//! Tree-sitter foundation shared by highlighting, scope context, and the
//! structural diff. Grammars are statically linked (no runtime loading, since
//! musl-static binaries cannot `dlopen`); a parse failure or unknown language
//! degrades silently to plain behavior so the UI is never blocked.

pub mod intraline;
pub mod registry;
pub mod scope;

use std::ops::Range;

use tree_sitter::Parser;

pub use registry::{HIGHLIGHT_NAMES, LangEntry, LanguageRegistry};
pub use scope::ScopeIndex;

/// Files larger than this are not parsed (avoids pathological cost on
/// generated/minified blobs); they degrade to plain rendering / textual diff.
pub(crate) const MAX_PARSE_BYTES: usize = 2_000_000;

/// Parse `src` with `entry`'s grammar. `None` on a language-setup or parse
/// failure so callers degrade gracefully.
pub(crate) fn parse(entry: &LangEntry, src: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&entry.language).ok()?;
    parser.parse(src, None)
}

/// `(start_byte, visible_end_byte)` per line, matching `str::lines()`: the
/// visible end excludes the trailing `\n`/`\r\n`. Shared by highlighting and
/// intra-line emphasis to map whole-file byte ranges onto individual lines.
pub(crate) fn line_bounds(content: &str) -> Vec<(usize, usize)> {
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            let end = if i > start && bytes.get(i - 1) == Some(&b'\r') {
                i - 1
            } else {
                i
            };
            out.push((start, end));
            start = i + 1;
        }
    }
    if start < bytes.len() {
        let end = if bytes.last() == Some(&b'\r') {
            bytes.len() - 1
        } else {
            bytes.len()
        };
        out.push((start, end));
    }
    out
}

/// Split a whole-file byte `range` across the lines it covers, calling `emit`
/// with each line's index and the range clamped and rebased to that line's
/// visible region. `starts` is the first column of `line_bounds`.
pub(crate) fn split_range_by_line(
    bounds: &[(usize, usize)],
    starts: &[usize],
    range: &Range<usize>,
    mut emit: impl FnMut(usize, Range<usize>),
) {
    let mut li = match starts.binary_search(&range.start) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    while let Some(&(ls, le)) = bounds.get(li) {
        if ls >= range.end {
            break;
        }
        let s = range.start.max(ls);
        let e = range.end.min(le);
        if s < e {
            emit(li, (s - ls)..(e - ls));
        }
        li += 1;
    }
}
