# M1 Core Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The headless review engine in `diffler-core`: compute a working-tree diff model with intra-line emphasis and stable hunk identity, hold comments + per-hunk verdicts in a session, persist it to `.diffler/`, and syntax-highlight file contents for the TUI to composite.

**Architecture:** Pure-logic crate, no terminal deps. `git2` produces deltas/hunks → `model.rs` types; `pairing.rs` pairs deleted/added lines and attaches byte-range emphasis from `diff.rs`; hunk identity = git blob hash of normalized hunk content; `session.rs` owns comments/verdicts and reconciles against fresh models; `store.rs` does atomic JSON persistence; `highlight.rs` wraps syntect/two-face returning per-line styled ranges. The TUI (next plan) only composites.

**Tech Stack:** git2 0.21, similar 3 (graphemes + ratio), syntect 5.2 + two-face 0.5, serde/serde_json, uuid v4, tempfile (tests).

**Conventions for every task:** run `just check` after each implementation step; the repo denies `unwrap`/`panic`/`todo` outside tests (tests may use `expect`). All imports at top of file. Comments only for non-obvious why. Commit messages: short, imperative, one line.

---

### Task 1: Diff model types

**Files:**
- Create: `crates/diffler-core/src/model.rs`
- Modify: `crates/diffler-core/src/lib.rs`

- [ ] **Step 1: Write the types and unit tests**

`crates/diffler-core/src/model.rs`:

```rust
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
}
```

`crates/diffler-core/src/lib.rs` — add module:

```rust
pub mod diff;
pub mod model;
pub mod repo;
```

- [ ] **Step 2: Run tests, verify pass**

Run: `cargo nextest run -p diffler-core model`
Expected: 5 tests pass.

- [ ] **Step 3: Run `just check`, fix any lints, commit**

```bash
git add crates/diffler-core
git commit -m "Add diff model types with content-hash hunk identity"
```

---

### Task 2: Intraline emphasis as byte ranges

The existing `diff::intraline` returns owned `Span` texts. The model stores byte ranges into the line text instead (so highlight + emphasis composite cleanly). Replace the Span API with a range API and port the tests.

**Files:**
- Rewrite: `crates/diffler-core/src/diff.rs`

- [ ] **Step 1: Rewrite `diff.rs`**

```rust
//! Intra-line diff: byte ranges of changed regions between a paired
//! old/new line, used for char-level emphasis on top of line diffs.

use std::ops::Range;

use similar::{ChangeTag, TextDiff};

/// Byte ranges (into each input) that differ between the two lines.
///
/// Returns `(old_emphasis, new_emphasis)`. Adjacent ranges are merged.
///
/// ```
/// use diffler_core::diff::intraline;
///
/// let (old, new) = intraline("if x < y:", "if x <= y:");
/// assert!(old.is_empty());
/// assert_eq!(new, vec![6..7]);
/// ```
pub fn intraline(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    // graphemes, not chars: emphasis must never split a combining sequence
    // or emoji cluster, or the TUI styles half a glyph
    let diff = TextDiff::from_graphemes(old, new);
    let mut old_ranges: Vec<Range<usize>> = Vec::new();
    let mut new_ranges: Vec<Range<usize>> = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;

    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                old_pos += len;
                new_pos += len;
            }
            ChangeTag::Delete => {
                push_range(&mut old_ranges, old_pos..old_pos + len);
                old_pos += len;
            }
            ChangeTag::Insert => {
                push_range(&mut new_ranges, new_pos..new_pos + len);
                new_pos += len;
            }
        }
    }

    (old_ranges, new_ranges)
}

fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if let Some(last) = ranges.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    ranges.push(range);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_lines_have_no_emphasis() {
        let (old, new) = intraline("same line", "same line");
        assert!(old.is_empty());
        assert!(new.is_empty());
    }

    #[test]
    fn ranges_are_in_bounds_and_ascending() {
        let old = "if claims.expiry < now():";
        let new = "if claims.expiry <= now() - LEEWAY:";
        let (old_r, new_r) = intraline(old, new);
        for r in &old_r {
            assert!(r.end <= old.len());
        }
        let mut prev_end = 0;
        for r in &new_r {
            assert!(r.start >= prev_end && r.end <= new.len());
            prev_end = r.end;
        }
    }

    #[test]
    fn insertion_is_emphasized_on_new_side_only() {
        let (old, new) = intraline("session.touch()", "session.touch(now())");
        assert!(old.is_empty());
        let joined: String = new
            .iter()
            .map(|r| &"session.touch(now())"[r.clone()])
            .collect();
        assert_eq!(joined, "now()");
    }

    #[test]
    fn adjacent_ranges_are_merged() {
        let (_, new) = intraline("ab", "aXYb");
        assert_eq!(new, vec![1..3]);
    }

    #[test]
    fn combining_characters_stay_whole() {
        // "e\u{301}" is one grapheme; emphasis must cover it atomically
        let new_line = "cafe\u{301}";
        let (_, new) = intraline("cafe", new_line);
        for r in &new {
            assert!(new_line.is_char_boundary(r.start), "range splits a char");
            assert!(new_line.is_char_boundary(r.end), "range splits a char");
        }
        let joined: String = new.iter().map(|r| &new_line[r.clone()]).collect();
        assert!(joined.contains('\u{301}'));
    }

    #[test]
    fn empty_inputs() {
        let (old, new) = intraline("", "");
        assert!(old.is_empty());
        assert!(new.is_empty());
    }
}
```

Note: the doctest in the old version asserted on `Span`s; the new doctest asserts ranges. The `Span` type is deleted — `model::DiffLine.emphasis` is the consumer now.

- [ ] **Step 2: Run tests + doctests**

Run: `cargo nextest run -p diffler-core diff && cargo test --doc -p diffler-core`
Expected: 6 unit tests + 1 doctest pass.

- [ ] **Step 3: `just check`, commit**

```bash
git add crates/diffler-core/src/diff.rs
git commit -m "Return intraline emphasis as byte ranges"
```

---

### Task 3: Git fixture helper for tests

Working-tree diff tests need real repos. Build the helper first.

**Files:**
- Create: `crates/diffler-core/tests/common/mod.rs`
- Create: `crates/diffler-core/tests/worktree_diff.rs` (just the mod hookup; tests come in Task 4)

- [ ] **Step 1: Write the fixture**

`crates/diffler-core/tests/common/mod.rs`:

```rust
use std::fs;
use std::path::Path;

use tempfile::TempDir;

/// A throwaway git repo with helpers to commit and mutate files.
pub struct Fixture {
    pub dir: TempDir,
    pub repo: git2::Repository,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init");
        let mut config = repo.config().expect("config");
        config.set_str("user.name", "test").expect("config");
        config.set_str("user.email", "test@test").expect("config");
        drop(config);
        Self { dir, repo }
    }

    pub fn write(&self, rel: &str, content: &str) {
        let path = self.dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, content).expect("write");
    }

    pub fn remove(&self, rel: &str) {
        fs::remove_file(self.dir.path().join(rel)).expect("remove");
    }

    pub fn commit_all(&self, message: &str) {
        let mut index = self.repo.index().expect("index");
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .expect("add");
        index.write().expect("index write");
        let tree_id = index.write_tree().expect("tree");
        let tree = self.repo.find_tree(tree_id).expect("find tree");
        let sig = self.repo.signature().expect("sig");
        let parent = self
            .repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        self.repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .expect("commit");
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }
}
```

`crates/diffler-core/tests/worktree_diff.rs`:

```rust
mod common;

use common::Fixture;

#[test]
fn fixture_smoke() {
    let fx = Fixture::new();
    fx.write("a.txt", "hello\n");
    fx.commit_all("base");
    assert!(fx.root().join(".git").exists());
}
```

- [ ] **Step 2: Run, verify pass**

Run: `cargo nextest run -p diffler-core fixture_smoke`
Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add crates/diffler-core/tests
git commit -m "Add git repo test fixture"
```

---

### Task 4: Working-tree diff engine

**Files:**
- Create: `crates/diffler-core/src/engine.rs`
- Modify: `crates/diffler-core/src/lib.rs`
- Modify: `crates/diffler-core/tests/worktree_diff.rs`

- [ ] **Step 1: Write failing integration tests**

Replace `crates/diffler-core/tests/worktree_diff.rs` with:

```rust
mod common;

use common::Fixture;
use diffler_core::engine::working_tree_diff;
use diffler_core::model::{FileStatus, LineKind};

#[test]
fn clean_tree_is_empty() {
    let fx = Fixture::new();
    fx.write("a.txt", "hello\n");
    fx.commit_all("base");
    let model = working_tree_diff(fx.root()).expect("diff");
    assert!(model.files.is_empty());
}

#[test]
fn modified_file_produces_hunk_with_line_numbers() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\ntwo\nthree\n");
    fx.commit_all("base");
    fx.write("a.txt", "one\nTWO\nthree\n");
    let model = working_tree_diff(fx.root()).expect("diff");
    assert_eq!(model.files.len(), 1);
    let file = &model.files[0];
    assert_eq!(file.path, "a.txt");
    assert_eq!(file.status, FileStatus::Modified);
    assert_eq!(file.old_text.as_deref(), Some("one\ntwo\nthree\n"));
    assert_eq!(file.new_text.as_deref(), Some("one\nTWO\nthree\n"));
    assert_eq!(file.hunks.len(), 1);
    let lines = &file.hunks[0].lines;
    let deleted: Vec<_> = lines.iter().filter(|l| l.kind == LineKind::Deleted).collect();
    let added: Vec<_> = lines.iter().filter(|l| l.kind == LineKind::Added).collect();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].text, "two");
    assert_eq!(deleted[0].old_no, Some(2));
    assert_eq!(deleted[0].new_no, None);
    assert_eq!(added[0].text, "TWO");
    assert_eq!(added[0].new_no, Some(2));
}

#[test]
fn untracked_file_is_included_as_added_lines() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    fx.write("new.txt", "alpha\nbeta\n");
    let model = working_tree_diff(fx.root()).expect("diff");
    let file = model
        .files
        .iter()
        .find(|f| f.path == "new.txt")
        .expect("untracked file present");
    assert_eq!(file.status, FileStatus::Untracked);
    let added: Vec<_> = file.hunks[0]
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(added, vec!["alpha", "beta"]);
}

#[test]
fn deleted_file_reported() {
    let fx = Fixture::new();
    fx.write("gone.txt", "bye\n");
    fx.commit_all("base");
    fx.remove("gone.txt");
    let model = working_tree_diff(fx.root()).expect("diff");
    let file = &model.files[0];
    assert_eq!(file.status, FileStatus::Deleted);
    assert!(file.new_text.is_none());
}

#[test]
fn staged_changes_are_included() {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("base");
    fx.write("a.txt", "ONE\n");
    // stage it
    let mut index = fx.repo.index().expect("index");
    index.add_path(std::path::Path::new("a.txt")).expect("add");
    index.write().expect("write");
    let model = working_tree_diff(fx.root()).expect("diff");
    assert_eq!(model.files.len(), 1);
    assert_eq!(model.files[0].status, FileStatus::Modified);
}

#[test]
fn binary_file_flagged_without_hunks() {
    let fx = Fixture::new();
    fx.write("a.txt", "x\n");
    fx.commit_all("base");
    std::fs::write(fx.root().join("blob.bin"), [0u8, 159, 146, 150]).expect("write");
    let model = working_tree_diff(fx.root()).expect("diff");
    let file = model
        .files
        .iter()
        .find(|f| f.path == "blob.bin")
        .expect("binary present");
    assert!(file.binary);
    assert!(file.hunks.is_empty());
}

#[test]
fn empty_repo_with_no_head_diffs_against_nothing() {
    let fx = Fixture::new();
    fx.write("first.txt", "hello\n");
    let model = working_tree_diff(fx.root()).expect("diff");
    let file = &model.files[0];
    assert_eq!(file.path, "first.txt");
    assert_eq!(file.status, FileStatus::Untracked);
}

#[test]
fn hunk_ids_survive_unrelated_edits() {
    let fx = Fixture::new();
    let base: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    fx.write("a.txt", &base);
    fx.commit_all("base");
    let edit_one = base.replace("line 5\n", "LINE FIVE\n");
    fx.write("a.txt", &edit_one);
    let m1 = working_tree_diff(fx.root()).expect("diff");
    let id_before = m1.files[0].hunks[0].id.clone();
    // unrelated edit far away creates a second hunk; first hunk id must not move
    let edit_two = edit_one.replace("line 35\n", "LINE THIRTY-FIVE\n");
    fx.write("a.txt", &edit_two);
    let m2 = working_tree_diff(fx.root()).expect("diff");
    assert!(m2.files[0].hunks.iter().any(|h| h.id == id_before));
    assert_eq!(m2.files[0].hunks.len(), 2);
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p diffler-core --test worktree_diff`
Expected: compile error — `engine` module doesn't exist.

- [ ] **Step 3: Implement the engine**

`crates/diffler-core/src/engine.rs`:

```rust
//! Working-tree diff computation: HEAD vs index + workdir, untracked included.

use std::path::Path;

use thiserror::Error;

use crate::model::{DiffLine, DiffModel, FileDiff, FileStatus, Hunk, LineKind, hunk_id};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Git(#[from] git2::Error),
    #[error("repository has no working directory")]
    NoWorkdir,
}

/// Diff HEAD against the working directory (with index), including
/// untracked files. This is the "what did the agent just do" view.
pub fn working_tree_diff(repo_root: &Path) -> Result<DiffModel, EngineError> {
    let repo = git2::Repository::open(repo_root)?;
    if repo.workdir().is_none() {
        return Err(EngineError::NoWorkdir);
    }

    let head_tree = match repo.head() {
        Ok(head) => Some(head.peel_to_tree()?),
        // unborn branch (fresh repo): diff everything against nothing
        Err(err) if err.code() == git2::ErrorCode::UnbornBranch => None,
        Err(err) => return Err(err.into()),
    };

    let mut opts = git2::DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .context_lines(3);

    let mut diff = repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
    diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;

    let mut files = Vec::new();
    let delta_count = diff.deltas().len();
    for idx in 0..delta_count {
        if let Some(file) = build_file(&repo, &diff, idx)? {
            files.push(file);
        }
    }
    Ok(DiffModel { files })
}

fn build_file(
    repo: &git2::Repository,
    diff: &git2::Diff<'_>,
    idx: usize,
) -> Result<Option<FileDiff>, EngineError> {
    let Some(patch) = git2::Patch::from_diff(diff, idx)? else {
        // binary or unreadable: fall back to delta metadata only
        return build_binary_file(diff, idx);
    };
    let delta = patch.delta();
    let path = delta_new_path(&delta);
    let old_path = delta
        .old_file()
        .path()
        .map(|p| p.to_string_lossy().into_owned());
    let status = map_status(delta.status());
    if delta.flags().is_binary() {
        return build_binary_file(diff, idx);
    }

    let old_text = blob_text(repo, delta.old_file().id());
    let new_text = workdir_text(repo, &path, delta.status());

    let mut hunks = Vec::new();
    for h in 0..patch.num_hunks() {
        let (hunk, line_count) = patch.hunk(h)?;
        let mut lines = Vec::with_capacity(line_count);
        for l in 0..line_count {
            let line = patch.line_in_hunk(h, l)?;
            let kind = match line.origin() {
                '-' => LineKind::Deleted,
                '+' => LineKind::Added,
                ' ' => LineKind::Context,
                // headers, EOF-newline markers etc. are not content lines
                _ => continue,
            };
            let text = String::from_utf8_lossy(line.content())
                .trim_end_matches(['\n', '\r'])
                .to_owned();
            lines.push(DiffLine::new(kind, line.old_lineno(), line.new_lineno(), text));
        }
        let id = hunk_id(&path, &lines)?;
        hunks.push(Hunk {
            id,
            old_start: hunk.old_start(),
            old_lines: hunk.old_lines(),
            new_start: hunk.new_start(),
            new_lines: hunk.new_lines(),
            lines,
        });
    }

    let old_path = if status == FileStatus::Renamed {
        old_path
    } else {
        None
    };
    Ok(Some(FileDiff {
        path,
        old_path,
        status,
        binary: false,
        old_text,
        new_text,
        hunks,
    }))
}

fn build_binary_file(
    diff: &git2::Diff<'_>,
    idx: usize,
) -> Result<Option<FileDiff>, EngineError> {
    let Some(delta) = diff.get_delta(idx) else {
        return Ok(None);
    };
    Ok(Some(FileDiff {
        path: delta_new_path(&delta),
        old_path: None,
        status: map_status(delta.status()),
        binary: true,
        old_text: None,
        new_text: None,
        hunks: Vec::new(),
    }))
}

fn delta_new_path(delta: &git2::DiffDelta<'_>) -> String {
    delta
        .new_file()
        .path()
        .or_else(|| delta.old_file().path())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn map_status(status: git2::Delta) -> FileStatus {
    match status {
        git2::Delta::Added => FileStatus::Added,
        git2::Delta::Deleted => FileStatus::Deleted,
        git2::Delta::Renamed => FileStatus::Renamed,
        git2::Delta::Untracked => FileStatus::Untracked,
        _ => FileStatus::Modified,
    }
}

fn blob_text(repo: &git2::Repository, oid: git2::Oid) -> Option<String> {
    if oid.is_zero() {
        return None;
    }
    let blob = repo.find_blob(oid).ok()?;
    if blob.is_binary() {
        return None;
    }
    String::from_utf8(blob.content().to_vec()).ok()
}

fn workdir_text(repo: &git2::Repository, rel: &str, status: git2::Delta) -> Option<String> {
    if status == git2::Delta::Deleted {
        return None;
    }
    let root = repo.workdir()?;
    std::fs::read_to_string(root.join(rel)).ok()
}
```

Add to `crates/diffler-core/src/lib.rs`:

```rust
pub mod diff;
pub mod engine;
pub mod model;
pub mod repo;
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo nextest run -p diffler-core --test worktree_diff`
Expected: all 8 tests pass. If `untracked` shows as `Added` instead of `Untracked` on some libgit2 versions, that's the delta status from `diff_tree_to_workdir_with_index` — fix by checking the test failure message and adjusting `map_status` only if needed; the test is the contract.

- [ ] **Step 5: `just check`, commit**

```bash
git add crates/diffler-core
git commit -m "Add working-tree diff engine"
```

---

### Task 5: Line pairing + emphasis enrichment

Pair deleted/added runs within each hunk (similarity-gated, delta-style) and fill `DiffLine.emphasis` via `diff::intraline`.

**Files:**
- Create: `crates/diffler-core/src/pairing.rs`
- Modify: `crates/diffler-core/src/lib.rs`
- Modify: `crates/diffler-core/src/engine.rs` (call enrichment)

- [ ] **Step 1: Write failing unit tests + implementation skeleton**

`crates/diffler-core/src/pairing.rs`:

```rust
//! Pair deleted/added line runs inside a hunk and attach intra-line
//! emphasis. Lines pair positionally within a run, gated by similarity,
//! mirroring delta's homologous-line model.

use similar::TextDiff;

use crate::diff::intraline;
use crate::model::{DiffModel, Hunk, LineKind};

/// Below this similarity the pair is treated as unrelated (no emphasis).
const MIN_SIMILARITY: f32 = 0.4;

pub fn enrich(model: &mut DiffModel) {
    for file in &mut model.files {
        for hunk in &mut file.hunks {
            enrich_hunk(hunk);
        }
    }
}

fn enrich_hunk(hunk: &mut Hunk) {
    let mut i = 0;
    while i < hunk.lines.len() {
        if hunk.lines[i].kind != LineKind::Deleted {
            i += 1;
            continue;
        }
        let del_start = i;
        while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Deleted {
            i += 1;
        }
        let add_start = i;
        while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Added {
            i += 1;
        }
        let pairs = (add_start - del_start).min(i - add_start);
        for p in 0..pairs {
            let (del_idx, add_idx) = (del_start + p, add_start + p);
            let old = hunk.lines[del_idx].text.clone();
            let new = hunk.lines[add_idx].text.clone();
            if similarity(&old, &new) < MIN_SIMILARITY {
                continue;
            }
            let (old_emphasis, new_emphasis) = intraline(&old, &new);
            hunk.lines[del_idx].emphasis = old_emphasis;
            hunk.lines[add_idx].emphasis = new_emphasis;
        }
    }
}

fn similarity(old: &str, new: &str) -> f32 {
    if old.is_empty() && new.is_empty() {
        return 1.0;
    }
    TextDiff::from_graphemes(old, new).ratio()
}

#[cfg(test)]
mod tests {
    use crate::model::{DiffLine, HunkId, LineKind};

    use super::*;

    fn hunk(lines: Vec<(LineKind, &str)>) -> Hunk {
        Hunk {
            id: HunkId("test".into()),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines: lines
                .into_iter()
                .map(|(k, t)| DiffLine::new(k, None, None, t.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn similar_pair_gets_emphasis_on_both_sides() {
        let mut h = hunk(vec![
            (LineKind::Context, "def f():"),
            (LineKind::Deleted, "    if x < y:"),
            (LineKind::Added, "    if x <= y:"),
        ]);
        enrich_hunk(&mut h);
        assert!(h.lines[1].emphasis.is_empty()); // deletion side: nothing removed, only insert
        assert_eq!(h.lines[2].emphasis, vec![10..11]);
    }

    #[test]
    fn dissimilar_pair_gets_no_emphasis() {
        let mut h = hunk(vec![
            (LineKind::Deleted, "totally_different_thing()"),
            (LineKind::Added, "x = 1"),
        ]);
        enrich_hunk(&mut h);
        assert!(h.lines[0].emphasis.is_empty());
        assert!(h.lines[1].emphasis.is_empty());
    }

    #[test]
    fn unbalanced_runs_pair_prefix_only() {
        let mut h = hunk(vec![
            (LineKind::Deleted, "alpha line one"),
            (LineKind::Deleted, "beta line two"),
            (LineKind::Added, "alpha line ONE"),
        ]);
        enrich_hunk(&mut h);
        assert!(!h.lines[2].emphasis.is_empty()); // paired with first deletion
        assert!(h.lines[1].emphasis.is_empty()); // unpaired deletion untouched
    }

    #[test]
    fn separate_runs_pair_independently() {
        let mut h = hunk(vec![
            (LineKind::Deleted, "first old line"),
            (LineKind::Added, "first new line"),
            (LineKind::Context, "middle"),
            (LineKind::Deleted, "second old line"),
            (LineKind::Added, "second new line"),
        ]);
        enrich_hunk(&mut h);
        assert!(!h.lines[0].emphasis.is_empty());
        assert!(!h.lines[1].emphasis.is_empty());
        assert!(!h.lines[3].emphasis.is_empty());
        assert!(!h.lines[4].emphasis.is_empty());
    }
}
```

Add `pub mod pairing;` to `lib.rs`. In `engine.rs`, at the end of `working_tree_diff` just before `Ok(DiffModel { files })`:

```rust
    let mut model = DiffModel { files };
    crate::pairing::enrich(&mut model);
    Ok(model)
```

(adjust the surrounding code accordingly — the function then ends with `Ok(model)`).

- [ ] **Step 2: Run all core tests**

Run: `cargo nextest run -p diffler-core`
Expected: all pass, including an emphasis now present in `worktree_diff` modified-file test scenarios (no assertion change needed — emphasis is additive).

- [ ] **Step 3: `just check`, commit**

```bash
git add crates/diffler-core
git commit -m "Pair changed lines and attach intra-line emphasis"
```

---

### Task 6: Session model — comments and verdicts

**Files:**
- Create: `crates/diffler-core/src/session.rs`
- Modify: `crates/diffler-core/src/lib.rs`
- Modify: `Cargo.toml` (workspace) + `crates/diffler-core/Cargo.toml` — add `uuid`

- [ ] **Step 1: Add uuid dependency**

Workspace `Cargo.toml` `[workspace.dependencies]`: add `uuid = { version = "1", features = ["v4"] }`.
`crates/diffler-core/Cargo.toml` `[dependencies]`: add `uuid.workspace = true`.

- [ ] **Step 2: Write the session module with tests**

`crates/diffler-core/src/session.rs`:

```rust
//! Review session: comments, per-hunk verdicts, reconciliation against
//! fresh diff models. Persistence lives in `store`.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::{DiffModel, HunkId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub verdict: Verdict,
    pub file: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentStatus {
    Open,
    Replied,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub author: String,
    pub body: String,
    pub at: u64,
}

/// Where a comment is anchored. `line` is the new-side line number unless
/// the line is a deletion, then it is the old-side number with `on_old_side`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub on_old_side: bool,
    #[serde(default)]
    pub hunk: Option<HunkId>,
    /// Snapshot of the anchored line's text, so the UI can mark the
    /// comment outdated when the agent rewrites the line.
    #[serde(default)]
    pub line_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub anchor: Anchor,
    pub body: String,
    pub status: CommentStatus,
    #[serde(default)]
    pub replies: Vec<Reply>,
    pub at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub verdicts: BTreeMap<HunkId, VerdictRecord>,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Session {
    pub fn add_comment(&mut self, author: &str, anchor: Anchor, body: &str) -> &Comment {
        self.comments.push(Comment {
            id: uuid::Uuid::new_v4().to_string(),
            author: author.to_owned(),
            anchor,
            body: body.to_owned(),
            status: CommentStatus::Open,
            replies: Vec::new(),
            at: now_unix(),
        });
        self.comments.last().expect("just pushed")
    }

    pub fn reply(&mut self, comment_id: &str, author: &str, body: &str) -> bool {
        let Some(comment) = self.comments.iter_mut().find(|c| c.id == comment_id) else {
            return false;
        };
        comment.replies.push(Reply {
            author: author.to_owned(),
            body: body.to_owned(),
            at: now_unix(),
        });
        if comment.status == CommentStatus::Open {
            comment.status = CommentStatus::Replied;
        }
        true
    }

    pub fn resolve(&mut self, comment_id: &str) -> bool {
        let Some(comment) = self.comments.iter_mut().find(|c| c.id == comment_id) else {
            return false;
        };
        comment.status = CommentStatus::Resolved;
        true
    }

    pub fn set_verdict(&mut self, hunk: HunkId, file: &str, verdict: Verdict, reason: Option<String>) {
        self.verdicts.insert(
            hunk,
            VerdictRecord {
                verdict,
                file: file.to_owned(),
                reason,
                at: now_unix(),
            },
        );
    }

    pub fn clear_verdict(&mut self, hunk: &HunkId) {
        self.verdicts.remove(hunk);
    }

    /// Drop verdicts whose hunk no longer exists in the model: a rewritten
    /// hunk has a new id and is genuinely new code, so it returns to pending.
    pub fn reconcile(&mut self, model: &DiffModel) {
        let live: std::collections::BTreeSet<&HunkId> = model
            .files
            .iter()
            .flat_map(|f| f.hunks.iter().map(|h| &h.id))
            .collect();
        self.verdicts.retain(|id, _| live.contains(id));
    }

    pub fn unresolved_comments(&self) -> impl Iterator<Item = &Comment> {
        self.comments
            .iter()
            .filter(|c| c.status != CommentStatus::Resolved)
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};

    use super::*;

    fn anchor(file: &str) -> Anchor {
        Anchor {
            file: file.to_owned(),
            line: Some(3),
            on_old_side: false,
            hunk: None,
            line_text: None,
        }
    }

    fn model_with_hunk(id: &str) -> DiffModel {
        DiffModel {
            files: vec![FileDiff {
                path: "a.txt".into(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                old_text: None,
                new_text: None,
                hunks: vec![Hunk {
                    id: HunkId(id.into()),
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    lines: vec![DiffLine::new(LineKind::Added, None, Some(1), "x".into())],
                }],
            }],
        }
    }

    #[test]
    fn comment_lifecycle_open_replied_resolved() {
        let mut s = Session::default();
        let id = s.add_comment("mattf", anchor("a.txt"), "why?").id.clone();
        assert_eq!(s.comments[0].status, CommentStatus::Open);
        assert!(s.reply(&id, "agent", "because"));
        assert_eq!(s.comments[0].status, CommentStatus::Replied);
        assert!(s.resolve(&id));
        assert_eq!(s.comments[0].status, CommentStatus::Resolved);
        assert_eq!(s.unresolved_comments().count(), 0);
    }

    #[test]
    fn reply_to_missing_comment_returns_false() {
        let mut s = Session::default();
        assert!(!s.reply("nope", "agent", "hi"));
    }

    #[test]
    fn verdict_set_and_clear() {
        let mut s = Session::default();
        let id = HunkId("h1".into());
        s.set_verdict(id.clone(), "a.txt", Verdict::Rejected, Some("wrong".into()));
        assert_eq!(s.verdicts[&id].verdict, Verdict::Rejected);
        s.clear_verdict(&id);
        assert!(s.verdicts.is_empty());
    }

    #[test]
    fn reconcile_keeps_live_hunks_and_drops_dead_ones() {
        let mut s = Session::default();
        s.set_verdict(HunkId("live".into()), "a.txt", Verdict::Accepted, None);
        s.set_verdict(HunkId("dead".into()), "a.txt", Verdict::Rejected, None);
        s.reconcile(&model_with_hunk("live"));
        assert!(s.verdicts.contains_key(&HunkId("live".into())));
        assert!(!s.verdicts.contains_key(&HunkId("dead".into())));
    }

    #[test]
    fn session_serializes_round_trip() {
        let mut s = Session::default();
        s.add_comment("mattf", anchor("a.txt"), "note");
        s.set_verdict(HunkId("h".into()), "a.txt", Verdict::Accepted, None);
        let json = serde_json::to_string(&s).expect("serialize");
        let back: Session = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back);
    }
}
```

Add `pub mod session;` to `lib.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p diffler-core session`
Expected: 5 tests pass.

- [ ] **Step 4: `just check`, commit**

```bash
git add Cargo.toml Cargo.lock crates/diffler-core
git commit -m "Add session model: comments, verdicts, reconciliation"
```

---

### Task 7: Persistence in .diffler/

**Files:**
- Create: `crates/diffler-core/src/store.rs`
- Modify: `crates/diffler-core/src/lib.rs`

- [ ] **Step 1: Write the store with tests**

`crates/diffler-core/src/store.rs`:

```rust
//! Session persistence: `.diffler/session.json` inside the repo,
//! atomically written, self-gitignored.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::session::Session;

const DIR: &str = ".diffler";
const FILE: &str = "session.json";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("corrupt session file {0}: {1}")]
    Corrupt(PathBuf, serde_json::Error),
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct OnDisk {
    version: u32,
    #[serde(flatten)]
    session: Session,
}

pub fn session_path(repo_root: &Path) -> PathBuf {
    repo_root.join(DIR).join(FILE)
}

pub fn load(repo_root: &Path) -> Result<Session, StoreError> {
    let path = session_path(repo_root);
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let on_disk: OnDisk =
                serde_json::from_str(&raw).map_err(|e| StoreError::Corrupt(path, e))?;
            Ok(on_disk.session)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Session::default()),
        Err(err) => Err(err.into()),
    }
}

/// Atomic write: temp file in the same directory, then rename, so a crash
/// mid-write never corrupts the session. The dir self-gitignores.
pub fn save(repo_root: &Path, session: &Session) -> Result<(), StoreError> {
    let dir = repo_root.join(DIR);
    fs::create_dir_all(&dir)?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "*\n")?;
    }
    let on_disk = OnDisk {
        version: 1,
        session: session.clone(),
    };
    let json = serde_json::to_string_pretty(&on_disk).map_err(std::io::Error::other)?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(json.as_bytes())?;
    tmp.persist(session_path(repo_root))
        .map_err(|e| StoreError::Io(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::model::HunkId;
    use crate::session::{Anchor, Verdict};

    use super::*;

    fn anchor() -> Anchor {
        Anchor {
            file: "a.txt".into(),
            line: Some(1),
            on_old_side: false,
            hunk: None,
            line_text: None,
        }
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = load(dir.path()).expect("load");
        assert_eq!(s, Session::default());
    }

    #[test]
    fn save_load_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = Session::default();
        s.add_comment("mattf", anchor(), "hm");
        s.set_verdict(HunkId("h".into()), "a.txt", Verdict::Accepted, None);
        save(dir.path(), &s).expect("save");
        let back = load(dir.path()).expect("load");
        assert_eq!(s, back);
    }

    #[test]
    fn save_writes_gitignore() {
        let dir = tempfile::tempdir().expect("tempdir");
        save(dir.path(), &Session::default()).expect("save");
        let gi = std::fs::read_to_string(dir.path().join(".diffler/.gitignore")).expect("read");
        assert_eq!(gi, "*\n");
    }

    #[test]
    fn corrupt_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".diffler")).expect("mkdir");
        std::fs::write(dir.path().join(".diffler/session.json"), "{not json").expect("write");
        assert!(matches!(load(dir.path()), Err(StoreError::Corrupt(..))));
    }
}
```

Note: `tempfile` is currently a dev-dependency of diffler-core; atomic save uses it at runtime. Move it: in `crates/diffler-core/Cargo.toml` add `tempfile.workspace = true` under `[dependencies]` (keep dev-dependencies entry removed — a regular dependency is visible to tests too).

Add `pub mod store;` to `lib.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p diffler-core store`
Expected: 4 tests pass.

- [ ] **Step 3: `just check`, commit**

```bash
git add crates/diffler-core
git commit -m "Persist sessions atomically in .diffler/"
```

---

### Task 8: Syntax highlighting module

Whole-file highlight, sliced per line — never per hunk (multi-line strings/comments would highlight incorrectly; see research/DIFF-RENDERING.md).

**Files:**
- Create: `crates/diffler-core/src/highlight.rs`
- Modify: `crates/diffler-core/src/lib.rs`
- Modify: `crates/diffler-core/Cargo.toml` — add syntect + two-face

- [ ] **Step 1: Move syntect/two-face deps from the bin to core**

`crates/diffler-core/Cargo.toml` `[dependencies]`: add `syntect.workspace = true` and `two-face.workspace = true`. Remove both from `crates/diffler/Cargo.toml` (the bin gets highlighting through core now; update its pre-staged comment accordingly).

- [ ] **Step 2: Write the module with tests**

`crates/diffler-core/src/highlight.rs`:

```rust
//! Whole-file syntax highlighting sliced into per-line styled ranges.
//! Highlighting whole files (not hunks) keeps stateful constructs like
//! multi-line strings correct across hunk boundaries.

use std::ops::Range;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

/// Foreground color + style for a byte range of one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledRange {
    pub range: Range<usize>,
    pub fg: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
}

impl Default for Highlighter {
    fn default() -> Self {
        let syntaxes = two_face::syntax::extra_newlines();
        let themes: EmbeddedLazyThemeSet = two_face::theme::extra();
        let theme = themes.get(EmbeddedThemeName::VisualStudioDarkPlus).clone();
        Self { syntaxes, theme }
    }
}

impl Highlighter {
    /// Highlight `content` as the language guessed from `path`'s extension.
    /// Returns one `Vec<StyledRange>` per line (without trailing newlines).
    /// Unknown languages produce empty ranges per line (plain rendering).
    pub fn highlight(&self, path: &str, content: &str) -> Vec<Vec<StyledRange>> {
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let Some(syntax) = self.syntaxes.find_syntax_by_extension(extension) else {
            return content.lines().map(|_| Vec::new()).collect();
        };
        let mut machine = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in syntect::util::LinesWithEndings::from(content) {
            let spans = machine
                .highlight_line(line, &self.syntaxes)
                .unwrap_or_default();
            let mut ranges = Vec::new();
            let mut pos = 0usize;
            let visible_len = line.trim_end_matches(['\n', '\r']).len();
            for (style, text) in spans {
                let start = pos;
                pos += text.len();
                let end = pos.min(visible_len);
                if start >= end {
                    continue;
                }
                ranges.push(StyledRange {
                    range: start..end,
                    fg: (style.foreground.r, style.foreground.g, style.foreground.b),
                    bold: style
                        .font_style
                        .contains(syntect::highlighting::FontStyle::BOLD),
                    italic: style
                        .font_style
                        .contains(syntect::highlighting::FontStyle::ITALIC),
                });
            }
            out.push(ranges);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_keywords_get_distinct_color() {
        let hl = Highlighter::default();
        let lines = hl.highlight("a.py", "def f():\n    return 1\n");
        assert_eq!(lines.len(), 2);
        // line styled with more than one color: keyword vs identifier
        let colors: std::collections::HashSet<(u8, u8, u8)> =
            lines[0].iter().map(|r| r.fg).collect();
        assert!(colors.len() > 1, "expected multiple colors, got {colors:?}");
    }

    #[test]
    fn ranges_cover_within_line_bounds() {
        let hl = Highlighter::default();
        let src = "fn main() { let x = \"hi\"; }\n";
        let lines = hl.highlight("a.rs", src);
        let visible = src.trim_end();
        for r in &lines[0] {
            assert!(r.range.end <= visible.len());
            assert!(r.range.start < r.range.end);
        }
    }

    #[test]
    fn multiline_string_state_carries_across_lines() {
        let hl = Highlighter::default();
        let src = "s = \"\"\"first\nsecond\nthird\"\"\"\nx = 1\n";
        let lines = hl.highlight("a.py", src);
        // the middle line is entirely inside the string: single styled run,
        // same color as the string-opening run on line 0
        let string_color = lines[0]
            .iter()
            .last()
            .map(|r| r.fg)
            .expect("line 0 styled");
        assert!(
            lines[1].iter().all(|r| r.fg == string_color),
            "inside-string line must keep string color"
        );
    }

    #[test]
    fn unknown_extension_yields_plain_lines() {
        let hl = Highlighter::default();
        let lines = hl.highlight("file.zzz-unknown", "a\nb\n");
        assert_eq!(lines, vec![Vec::new(), Vec::new()]);
    }
}
```

Add `pub mod highlight;` to `lib.rs`.

Theme note: `EmbeddedThemeName::VisualStudioDarkPlus` is the closest bundled match to GitHub-dark in two-face. If the variant name differs at compile time, list available variants with `EmbeddedThemeName` docs (`cargo doc -p two-face --open` or docs.rs) and pick the closest dark variant; the tests assert behavior, not specific colors. The real GitHub-dark `.tmTheme` ships in M2's theme work.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p diffler-core highlight`
Expected: 4 tests pass.

- [ ] **Step 4: `just check`, commit**

```bash
git add Cargo.toml Cargo.lock crates
git commit -m "Add whole-file syntax highlighting sliced per line"
```

---

### Task 9: Engine facade + workspace gate

Tie it together: one call the TUI/MCP layers will use, and a final green gate.

**Files:**
- Modify: `crates/diffler-core/src/lib.rs`
- Create: `crates/diffler-core/tests/facade.rs`

- [ ] **Step 1: Write the failing facade test**

`crates/diffler-core/tests/facade.rs`:

```rust
mod common;

use common::Fixture;
use diffler_core::review::Review;
use diffler_core::session::Verdict;

#[test]
fn review_refresh_reconciles_verdicts_and_persists() {
    let fx = Fixture::new();
    fx.write("a.py", "def f():\n    return 1\n");
    fx.commit_all("base");
    fx.write("a.py", "def f():\n    return 2\n");

    let mut review = Review::open(fx.root()).expect("open");
    assert_eq!(review.model.files.len(), 1);
    let hunk_id = review.model.files[0].hunks[0].id.clone();

    review
        .session
        .set_verdict(hunk_id.clone(), "a.py", Verdict::Rejected, Some("why 2?".into()));
    review.save().expect("save");

    // unrelated refresh keeps the verdict
    let mut review = Review::open(fx.root()).expect("reopen");
    assert!(review.session.verdicts.contains_key(&hunk_id));

    // the agent rewrites the hunk: verdict resets to pending
    fx.write("a.py", "def f():\n    return 3\n");
    review.refresh().expect("refresh");
    assert!(!review.session.verdicts.contains_key(&hunk_id));
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p diffler-core --test facade`
Expected: compile error — `review` module missing.

- [ ] **Step 3: Implement the facade**

Create `crates/diffler-core/src/review.rs`:

```rust
//! Facade tying engine, session, and store together: the one entry point
//! the TUI and MCP layers consume.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::engine::{EngineError, working_tree_diff};
use crate::model::DiffModel;
use crate::session::Session;
use crate::store::{self, StoreError};

#[derive(Debug, Error)]
pub enum ReviewError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct Review {
    pub repo_root: PathBuf,
    pub model: DiffModel,
    pub session: Session,
}

impl Review {
    /// Load the persisted session (if any), compute the current working-tree
    /// diff, and reconcile verdicts against it.
    pub fn open(repo_root: &Path) -> Result<Self, ReviewError> {
        let model = working_tree_diff(repo_root)?;
        let mut session = store::load(repo_root)?;
        session.reconcile(&model);
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            model,
            session,
        })
    }

    /// Recompute the diff (the watcher calls this on changes) and drop
    /// verdicts for hunks that no longer exist.
    pub fn refresh(&mut self) -> Result<(), ReviewError> {
        self.model = working_tree_diff(&self.repo_root)?;
        self.session.reconcile(&self.model);
        Ok(())
    }

    pub fn save(&self) -> Result<(), ReviewError> {
        store::save(&self.repo_root, &self.session)?;
        Ok(())
    }
}
```

`lib.rs` final module list:

```rust
pub mod diff;
pub mod engine;
pub mod highlight;
pub mod model;
pub mod pairing;
pub mod repo;
pub mod review;
pub mod session;
pub mod store;
```

- [ ] **Step 4: Run the full gate**

Run: `just ci`
Expected: everything green (fmt, clippy `-D warnings`, all nextest tests across both crates, doctests).

- [ ] **Step 5: Update PLAN.md status line**

In `PLAN.md`, replace the Status paragraph with:

```markdown
Core engine (M1 part 1) done: working-tree diff with intra-line emphasis and
stable hunk identity, sessions (comments + verdicts) persisted in `.diffler/`,
whole-file syntax highlighting. Next: M1 TUI, then M1 MCP server.
```

- [ ] **Step 6: Commit and push**

```bash
git add -A
git commit -m "Add review facade tying engine, session, and store"
git push
```

Watch CI: `gh run watch $(gh run list --branch main --workflow CI --limit 1 --json databaseId -q '.[0].databaseId') --exit-status`
Expected: all 8 jobs green.

---

## Self-review notes

- Spec coverage (M1 core scope): working-tree diff ✓ (Task 4), untracked/staged/deleted/binary/unborn-HEAD edge cases ✓ (Task 4 tests), histogram-quality pairing + char emphasis ✓ (Tasks 2, 5), hunk content-hash identity + verdict reconciliation ✓ (Tasks 1, 6, 9), comments with open/replied/resolved lifecycle ✓ (Task 6), `.diffler/` atomic persistence + self-gitignore ✓ (Task 7), whole-file-then-slice highlighting ✓ (Task 8). NOT in this plan (by design): watcher, TUI screens, MCP tools — next two plans.
- Type consistency check: `HunkId` (Tasks 1/6/9), `DiffLine.emphasis: Vec<Range<usize>>` (Tasks 1/2/5), `Session`/`store::{load,save}` signatures (Tasks 6/7/9) — all cross-referenced.
- git2 API risk: `Patch::hunk` tuple shape and `DiffFindOptions` builder are per git2 0.21 docs; if a signature differs at compile time, consult docs.rs/git2/0.21 — the tests define behavior, adjust call sites only.
