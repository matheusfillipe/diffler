//! Diff model: files, hunks, lines.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// FNV-1a 64 as lowercase hex. Persisted viewed marks key on it, so the
/// algorithm must never change.
fn stable_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffModel {
    pub files: Vec<FileDiff>,
}

impl DiffModel {
    /// Content identity of the whole model, so a refresh that recomputed an
    /// identical diff keeps derived state.
    pub fn fingerprint(&self) -> String {
        let mut buf = Vec::new();
        for file in &self.files {
            buf.extend_from_slice(file.path.as_bytes());
            buf.push(0);
            buf.extend_from_slice(file.sides_hash().as_bytes());
            buf.push(b'\n');
        }
        stable_hash(&buf)
    }

    pub fn find_line(&self, file: &str, line: u32, on_old_side: bool) -> Option<&DiffLine> {
        let file = self.files.iter().find(|f| f.path == file)?;
        file.hunks.iter().flat_map(|h| &h.lines).find(|l| {
            let no = if on_old_side { l.old_no } else { l.new_no };
            no == Some(line)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    /// Full contents of each side. `None` for binary files and for the
    /// missing side of adds/deletes.
    pub old_text: Option<String>,
    pub new_text: Option<String>,
    pub hunks: Vec<Hunk>,
    pub hashes: HashCache,
    pub blobs: BlobIds,
}

/// Hex blob ids of a file's two sides. A working-tree side's id has no
/// object behind it in the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobIds {
    pub old: Option<String>,
    pub new: Option<String>,
}

/// Memo for [`FileDiff::content_hash`] and [`FileDiff::sides_hash`], which
/// the UI reads every frame. Always compares equal so `FileDiff` equality is
/// on data.
#[derive(Debug, Clone, Default)]
pub struct HashCache {
    content: std::sync::OnceLock<String>,
    sides: std::sync::OnceLock<String>,
}

impl PartialEq for HashCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for HashCache {}

impl FileDiff {
    /// Identity of the new side, which viewed marks key on. A binary file
    /// uses its new blob id.
    pub fn content_hash(&self) -> String {
        self.hashes
            .content
            .get_or_init(|| match (&self.new_text, &self.blobs.new) {
                (None, Some(blob)) => blob.clone(),
                (text, _) => stable_hash(text.as_deref().unwrap_or("").as_bytes()),
            })
            .clone()
    }

    /// `(added, deleted)` line counts.
    pub fn diffstat(&self) -> (usize, usize) {
        let mut added = 0;
        let mut deleted = 0;
        for line in self.hunks.iter().flat_map(|h| &h.lines) {
            match line.kind {
                LineKind::Added => added += 1,
                LineKind::Deleted => deleted += 1,
                LineKind::Context => {}
            }
        }
        (added, deleted)
    }

    /// Identity of both sides, for caches derived from old and new text.
    pub fn sides_hash(&self) -> String {
        self.hashes
            .sides
            .get_or_init(|| {
                let mut bytes = Vec::from(self.old_text.as_deref().unwrap_or("").as_bytes());
                bytes.push(0);
                bytes.extend_from_slice(self.new_text.as_deref().unwrap_or("").as_bytes());
                for blob in [&self.blobs.old, &self.blobs.new] {
                    bytes.push(0);
                    bytes.extend_from_slice(blob.as_deref().unwrap_or("").as_bytes());
                }
                stable_hash(&bytes)
            })
            .clone()
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
    /// A file a walkthrough anchors outside the diff.
    Unchanged,
}

impl FileStatus {
    /// Each status has a distinct shape so it reads without colour.
    pub const fn glyph(self) -> char {
        match self {
            Self::Added => '+',
            Self::Modified => '●',
            Self::Deleted => '−',
            Self::Renamed => '~',
            Self::Untracked => '○',
            Self::Unchanged => '·',
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Added => "new file",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Untracked => "untracked",
            Self::Unchanged => "unchanged",
        }
    }
}

/// Hash of a hunk's content, so it survives edits elsewhere in the file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HunkId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub id: HunkId,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// git's function heading after the second `@@`, empty when git gives
    /// none. `id` ignores it.
    pub context: String,
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

impl LineKind {
    pub const fn origin(self) -> char {
        match self {
            Self::Context => ' ',
            Self::Deleted => '-',
            Self::Added => '+',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// Without the trailing newline.
    pub text: String,
    /// Byte ranges within `text`.
    pub emphasis: Vec<Range<usize>>,
    /// A paired line the structural algorithm found differs in layout alone.
    pub reformat_only: bool,
}

impl DiffLine {
    pub fn new(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: String) -> Self {
        Self {
            kind,
            old_no,
            new_no,
            text,
            emphasis: Vec::new(),
            reformat_only: false,
        }
    }

    pub const fn number_on(&self, old_side: bool) -> Option<u32> {
        if old_side { self.old_no } else { self.new_no }
    }
}

/// `occurrence` counts earlier hunks in the file with the same content, so
/// identical hunks get distinct ids. Prefer [`disambiguated_hunk_id`].
pub fn hunk_id(file_path: &str, lines: &[DiffLine], occurrence: usize) -> HunkId {
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
    if occurrence > 0 {
        buf.push_str(&occurrence.to_string());
        buf.push('\n');
    }
    HunkId(stable_hash(buf.as_bytes()))
}

/// [`hunk_id`] with `seen` counting occurrences across one file's hunks.
// every caller shares one default-hashed map for one file's hunks, so a
// generic hasher buys nothing
#[allow(clippy::implicit_hasher)]
pub fn disambiguated_hunk_id(
    file_path: &str,
    lines: &[DiffLine],
    seen: &mut std::collections::HashMap<HunkId, usize>,
) -> HunkId {
    let base = hunk_id(file_path, lines, 0);
    let occurrence = seen.entry(base).or_insert(0);
    let id = hunk_id(file_path, lines, *occurrence);
    *occurrence += 1;
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_is_fnv1a64_and_never_changes() {
        assert_eq!(stable_hash(b""), "cbf29ce484222325");
        assert_eq!(stable_hash(b"hello"), "a430d84680aabd0b");
    }

    #[test]
    fn file_status_glyph_and_label_cover_all_variants() {
        let glyphs = [
            FileStatus::Added,
            FileStatus::Modified,
            FileStatus::Deleted,
            FileStatus::Renamed,
            FileStatus::Untracked,
            FileStatus::Unchanged,
        ]
        .map(FileStatus::glyph);
        assert_eq!(glyphs, ['+', '●', '−', '~', '○', '·']);
        let mut distinct = glyphs.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            glyphs.len(),
            "no two statuses share a shape"
        );

        assert_eq!(FileStatus::Added.label(), "new file");
        assert_eq!(FileStatus::Modified.label(), "modified");
        assert_eq!(FileStatus::Deleted.label(), "deleted");
        assert_eq!(FileStatus::Renamed.label(), "renamed");
        assert_eq!(FileStatus::Untracked.label(), "untracked");
        assert_eq!(FileStatus::Unchanged.label(), "unchanged");
    }

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine::new(kind, None, None, text.to_owned())
    }

    #[test]
    fn hunk_id_is_stable() {
        let lines = vec![line(LineKind::Deleted, "a"), line(LineKind::Added, "b")];
        let id1 = hunk_id("src/x.rs", &lines, 0);
        let id2 = hunk_id("src/x.rs", &lines, 0);
        assert_eq!(id1, id2);
    }

    #[test]
    fn hunk_id_changes_with_content() {
        let a = vec![line(LineKind::Added, "x")];
        let b = vec![line(LineKind::Added, "y")];
        assert_ne!(hunk_id("f", &a, 0), hunk_id("f", &b, 0));
    }

    #[test]
    fn hunk_id_changes_with_kind() {
        let a = vec![line(LineKind::Added, "x")];
        let b = vec![line(LineKind::Deleted, "x")];
        assert_ne!(hunk_id("f", &a, 0), hunk_id("f", &b, 0));
    }

    #[test]
    fn hunk_id_changes_with_file() {
        let lines = vec![line(LineKind::Added, "x")];
        assert_ne!(hunk_id("a", &lines, 0), hunk_id("b", &lines, 0));
    }

    #[test]
    fn hunk_id_changes_with_occurrence() {
        let lines = vec![line(LineKind::Added, "x")];
        assert_ne!(hunk_id("f", &lines, 0), hunk_id("f", &lines, 1));
    }

    #[test]
    fn disambiguated_hunk_id_gives_identical_hunks_distinct_ids() {
        let lines = vec![line(LineKind::Added, "x")];
        let mut seen = std::collections::HashMap::new();
        let first = disambiguated_hunk_id("f", &lines, &mut seen);
        let second = disambiguated_hunk_id("f", &lines, &mut seen);
        assert_ne!(first, second);
        assert_eq!(first, hunk_id("f", &lines, 0));
        assert_eq!(second, hunk_id("f", &lines, 1));
    }

    #[test]
    fn header_formats() {
        let hunk = Hunk {
            id: HunkId("h".into()),
            old_start: 10,
            old_lines: 7,
            new_start: 10,
            new_lines: 9,
            context: String::new(),
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
            hashes: HashCache::default(),
            blobs: BlobIds::default(),
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
            hashes: HashCache::default(),
            blobs: BlobIds::default(),
        };
        assert_eq!(file.content_hash(), file.content_hash());
    }

    #[test]
    fn sides_hash_changes_when_old_text_changes() {
        let base = FileDiff {
            path: "f.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            old_text: Some("fn main() {}".into()),
            new_text: Some("fn main() { let x = 1; }".into()),
            hunks: vec![],
            hashes: HashCache::default(),
            blobs: BlobIds::default(),
        };
        let mut changed = base.clone();
        changed.old_text = Some("fn main() { unreachable!() }".into());
        assert_eq!(
            base.content_hash(),
            changed.content_hash(),
            "same new side, same content hash"
        );
        assert_ne!(base.sides_hash(), changed.sides_hash());
    }

    fn one_file_model(path: &str, old_text: &str, new_text: &str) -> DiffModel {
        DiffModel {
            files: vec![FileDiff {
                path: path.to_owned(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                old_text: Some(old_text.to_owned()),
                new_text: Some(new_text.to_owned()),
                hunks: vec![],
                hashes: HashCache::default(),
                blobs: BlobIds::default(),
            }],
        }
    }

    #[test]
    fn fingerprint_is_stable_for_identical_models() {
        let a = one_file_model("f.rs", "old", "new");
        let b = one_file_model("f.rs", "old", "new");
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_changes_with_content_path_and_file_set() {
        let base = one_file_model("f.rs", "old", "new");
        assert_ne!(
            base.fingerprint(),
            one_file_model("f.rs", "old", "newer").fingerprint(),
            "changed side changes the fingerprint"
        );
        assert_ne!(
            base.fingerprint(),
            one_file_model("g.rs", "old", "new").fingerprint(),
            "renamed file changes the fingerprint"
        );
        let mut grown = base.clone();
        grown.files.extend(one_file_model("g.rs", "", "x").files);
        assert_ne!(
            base.fingerprint(),
            grown.fingerprint(),
            "added file changes the fingerprint"
        );
    }

    fn model_with_lines() -> DiffModel {
        DiffModel {
            files: vec![FileDiff {
                path: "f.rs".into(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                old_text: None,
                new_text: None,
                hunks: vec![Hunk {
                    id: HunkId("h".into()),
                    old_start: 1,
                    old_lines: 2,
                    new_start: 1,
                    new_lines: 2,
                    context: String::new(),
                    lines: vec![
                        DiffLine::new(LineKind::Context, Some(1), Some(1), "one".into()),
                        DiffLine::new(LineKind::Deleted, Some(2), None, "two".into()),
                        DiffLine::new(LineKind::Added, None, Some(2), "TWO".into()),
                    ],
                }],
                hashes: HashCache::default(),
                blobs: BlobIds::default(),
            }],
        }
    }

    #[test]
    fn diffstat_counts_added_and_deleted_over_hunks() {
        let model = model_with_lines();
        assert_eq!(model.files[0].diffstat(), (1, 1));

        let file = FileDiff {
            path: "f.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            old_text: None,
            new_text: None,
            hunks: vec![
                Hunk {
                    id: HunkId("a".into()),
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 2,
                    context: String::new(),
                    lines: vec![
                        DiffLine::new(LineKind::Context, Some(1), Some(1), "ctx".into()),
                        DiffLine::new(LineKind::Added, None, Some(2), "add one".into()),
                    ],
                },
                Hunk {
                    id: HunkId("b".into()),
                    old_start: 5,
                    old_lines: 1,
                    new_start: 6,
                    new_lines: 1,
                    context: String::new(),
                    lines: vec![
                        DiffLine::new(LineKind::Deleted, Some(5), None, "gone".into()),
                        DiffLine::new(LineKind::Added, None, Some(6), "add two".into()),
                    ],
                },
            ],
            hashes: HashCache::default(),
            blobs: BlobIds::default(),
        };
        assert_eq!(file.diffstat(), (2, 1));
    }

    #[test]
    fn find_line_matches_the_requested_side() {
        let model = model_with_lines();
        let new_side = model.find_line("f.rs", 2, false).expect("new side");
        assert_eq!(new_side.text, "TWO");
        let old_side = model.find_line("f.rs", 2, true).expect("old side");
        assert_eq!(old_side.text, "two");
    }

    #[test]
    fn find_line_misses_unknown_files_and_lines() {
        let model = model_with_lines();
        assert!(model.find_line("nope.rs", 1, false).is_none());
        assert!(model.find_line("f.rs", 99, false).is_none());
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
            hashes: HashCache::default(),
            blobs: BlobIds::default(),
        };
        let hash = file.content_hash();
        assert!(!hash.is_empty());
    }
}
