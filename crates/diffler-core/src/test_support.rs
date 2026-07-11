//! Shared test fixtures for review-state unit tests: anchor and file-diff
//! builders reused by the session, store, and feedback test modules.

use crate::model::{FileDiff, FileStatus, HashCache};
use crate::session::Anchor;

pub(crate) fn anchor(file: &str, line: Option<u32>) -> Anchor {
    Anchor {
        file: file.to_owned(),
        line,
        line_end: None,
        on_old_side: false,
        line_text: None,
    }
}

pub(crate) fn file_diff(path: &str, new_text: &str) -> FileDiff {
    FileDiff {
        path: path.to_owned(),
        old_path: None,
        status: FileStatus::Modified,
        binary: false,
        old_text: None,
        new_text: Some(new_text.to_owned()),
        hunks: Vec::new(),
        hashes: HashCache::default(),
    }
}
