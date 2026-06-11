//! Diff model: what changed, organized as files -> hunks -> lines.

use std::ops::Range;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffModel {
    pub files: Vec<FileDiff>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    /// Full contents of each side, used for whole-file syntax highlighting.
    /// `None` for binary files and for the missing side of adds/deletes.
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// Content identity of the new side, used for viewed-mark invalidation.
    pub fn content_hash(&self) -> String {
        let bytes = self.new_text.as_deref().unwrap_or("").as_bytes();
        git2::Oid::hash_object(git2::ObjectType::Blob, bytes)
            .map(|o| o.to_string())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

/// Stable identity for a hunk: hash of its normalized content. Survives
/// edits elsewhere in the file; changes when the hunk's lines change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HunkId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub id: HunkId,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    pub fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Deleted,
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// Line content without the trailing newline.
    pub text: String,
    /// Byte ranges within `text` to emphasize (intra-line changes).
    pub emphasis: Vec<Range<usize>>,
}

impl DiffLine {
    pub fn new(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: String) -> Self {
        Self {
            kind,
            old_no,
            new_no,
            text,
            emphasis: Vec::new(),
        }
    }
}

/// Hash the hunk's content (kinds + text) into a stable id using git's
/// blob hashing, so no extra hash dependency is needed.
pub fn hunk_id(file_path: &str, lines: &[DiffLine]) -> Result<HunkId, git2::Error> {
    let mut buf = String::new();
    buf.push_str(file_path);
    buf.push('\n');
    for line in lines {
        let tag = match line.kind {
            LineKind::Context => ' ',
            LineKind::Deleted => '-',
            LineKind::Added => '+',
        };
        buf.push(tag);
        buf.push_str(&line.text);
        buf.push('\n');
    }
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, buf.as_bytes())?;
    Ok(HunkId(oid.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine::new(kind, None, None, text.to_owned())
    }

    #[test]
    fn hunk_id_is_stable() {
        let lines = vec![line(LineKind::Deleted, "a"), line(LineKind::Added, "b")];
        let id1 = hunk_id("src/x.rs", &lines).expect("hash");
        let id2 = hunk_id("src/x.rs", &lines).expect("hash");
        assert_eq!(id1, id2);
    }

    #[test]
    fn hunk_id_changes_with_content() {
        let a = vec![line(LineKind::Added, "x")];
        let b = vec![line(LineKind::Added, "y")];
        assert_ne!(
            hunk_id("f", &a).expect("hash"),
            hunk_id("f", &b).expect("hash")
        );
    }

    #[test]
    fn hunk_id_changes_with_kind() {
        let a = vec![line(LineKind::Added, "x")];
        let b = vec![line(LineKind::Deleted, "x")];
        assert_ne!(
            hunk_id("f", &a).expect("hash"),
            hunk_id("f", &b).expect("hash")
        );
    }

    #[test]
    fn hunk_id_changes_with_file() {
        let lines = vec![line(LineKind::Added, "x")];
        assert_ne!(
            hunk_id("a", &lines).expect("hash"),
            hunk_id("b", &lines).expect("hash")
        );
    }

    #[test]
    fn header_formats() {
        let hunk = Hunk {
            id: HunkId("h".into()),
            old_start: 10,
            old_lines: 7,
            new_start: 10,
            new_lines: 9,
            lines: vec![],
        };
        assert_eq!(hunk.header(), "@@ -10,7 +10,9 @@");
    }

    #[test]
    fn content_hash_changes_when_new_text_changes() {
        let base = FileDiff {
            path: "f.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            old_text: None,
            new_text: Some("fn main() {}".into()),
            hunks: vec![],
        };
        let mut changed = base.clone();
        changed.new_text = Some("fn main() { let x = 1; }".into());
        assert_ne!(base.content_hash(), changed.content_hash());
    }

    #[test]
    fn content_hash_is_stable() {
        let file = FileDiff {
            path: "f.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            old_text: None,
            new_text: Some("same content".into()),
            hunks: vec![],
        };
        assert_eq!(file.content_hash(), file.content_hash());
    }

    #[test]
    fn content_hash_falls_back_for_none() {
        let file = FileDiff {
            path: "f.rs".into(),
            old_path: None,
            status: FileStatus::Deleted,
            binary: false,
            old_text: None,
            new_text: None,
            hunks: vec![],
        };
        // must not panic, must return a non-empty string (git hash of empty blob)
        let hash = file.content_hash();
        assert!(!hash.is_empty());
    }
}
