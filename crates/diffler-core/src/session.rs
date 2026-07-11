//! Review session: comments and per-file viewed marks, reconciled against
//! fresh diff models. Persistence lives in `store`.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::DiffModel;

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
    /// Forge-side id once synced/posted; `None` for purely local replies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<String>,
}

/// Where a comment is anchored. `line` (and `line_end` for visual ranges)
/// is the new-side line number unless the line is a deletion, then it is
/// the old-side number with `on_old_side`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    #[serde(default)]
    pub on_old_side: bool,
    /// Snapshot of the anchored line's text, so the UI can mark the
    /// comment outdated when the agent rewrites the line.
    #[serde(default)]
    pub line_text: Option<String>,
}

impl Anchor {
    /// Whether the anchor no longer matches the model. Range comments
    /// anchor to their end line: that is the line whose disappearance or
    /// `line_text` drift marks them outdated. A line-less anchor is
    /// outdated only once the whole file leaves the diff.
    pub fn is_outdated(&self, model: &DiffModel) -> bool {
        match self.line_end.or(self.line) {
            Some(line) => match model.find_line(&self.file, line, self.on_old_side) {
                Some(found) => self
                    .line_text
                    .as_deref()
                    .is_some_and(|snap| snap != found.text),
                None => true,
            },
            None => !model.files.iter().any(|f| f.path == self.file),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: String,
    /// Forge-side id once synced/posted; `None` for purely local comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<String>,
    /// The forge's review-thread handle, where the forge has one — what
    /// thread resolution posts against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
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
    /// Per-file viewed marks: path -> content hash of the new side at the
    /// time of marking. A changed hash means the file needs re-review.
    #[serde(default)]
    pub viewed: BTreeMap<String, String>,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Session {
    pub fn add_comment(&mut self, anchor: Anchor, author: &str, body: &str) -> &Comment {
        self.comments.push(Comment {
            remote_id: None,
            thread_id: None,
            id: uuid::Uuid::new_v4().to_string(),
            author: author.to_owned(),
            anchor,
            body: body.to_owned(),
            status: CommentStatus::Open,
            replies: Vec::new(),
            at: now_unix(),
        });
        // just pushed, so the vec is non-empty
        #[allow(clippy::expect_used)]
        self.comments.last().expect("just pushed")
    }

    fn comment_mut(&mut self, comment_id: &str) -> Option<&mut Comment> {
        self.comments.iter_mut().find(|c| c.id == comment_id)
    }

    /// Remove the comment with `id`; `true` when something was deleted.
    pub fn delete_comment(&mut self, id: &str) -> bool {
        let before = self.comments.len();
        self.comments.retain(|c| c.id != id);
        self.comments.len() != before
    }

    pub fn reply(&mut self, comment_id: &str, author: &str, body: &str) -> bool {
        let Some(comment) = self.comment_mut(comment_id) else {
            return false;
        };
        comment.replies.push(Reply {
            remote_id: None,
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
        let Some(comment) = self.comment_mut(comment_id) else {
            return false;
        };
        comment.status = CommentStatus::Resolved;
        true
    }

    /// Replace a comment's body in place (status, replies, and anchor are
    /// kept). No author: an edit corrects the existing comment, it doesn't
    /// attribute a new one.
    pub fn edit_comment(&mut self, comment_id: &str, body: &str) -> bool {
        let Some(comment) = self.comment_mut(comment_id) else {
            return false;
        };
        body.clone_into(&mut comment.body);
        true
    }

    pub fn mark_viewed(&mut self, path: &str, hash: &str) {
        self.viewed.insert(path.to_owned(), hash.to_owned());
    }

    pub fn unmark_viewed(&mut self, path: &str) {
        self.viewed.remove(path);
    }

    /// A stale hash means the file changed since it was marked: not viewed
    /// anymore (auto-reset semantics).
    pub fn is_viewed(&self, path: &str, current_hash: &str) -> bool {
        self.viewed.get(path).is_some_and(|h| h == current_hash)
    }

    /// Drop viewed marks for files that left the diff or whose content
    /// changed since marking. Comments are kept: they stay useful (possibly
    /// flagged outdated) even when their file moves on.
    pub fn reconcile(&mut self, model: &DiffModel) {
        let live: BTreeMap<&str, String> = model
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.content_hash()))
            .collect();
        self.viewed
            .retain(|path, hash| live.get(path.as_str()).is_some_and(|h| h == hash));
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{DiffLine, FileDiff, FileStatus, Hunk, HunkId, LineKind};
    use crate::test_support::{anchor, file_diff};

    use super::*;

    fn model(files: Vec<FileDiff>) -> DiffModel {
        DiffModel { files }
    }

    #[test]
    fn comment_lifecycle_open_replied_resolved() {
        let mut s = Session::default();
        let id = s
            .add_comment(anchor("a.txt", Some(3)), "reviewer", "why?")
            .id
            .clone();
        assert_eq!(s.comments[0].status, CommentStatus::Open);
        assert!(s.reply(&id, "agent", "because"));
        assert_eq!(s.comments[0].status, CommentStatus::Replied);
        assert!(s.resolve(&id));
        assert_eq!(s.comments[0].status, CommentStatus::Resolved);
        assert!(
            s.comments
                .iter()
                .all(|c| c.status == CommentStatus::Resolved)
        );
    }

    #[test]
    fn reply_to_missing_comment_returns_false() {
        let mut s = Session::default();
        assert!(!s.reply("nope", "agent", "hi"));
    }

    #[test]
    fn resolve_missing_comment_returns_false() {
        let mut s = Session::default();
        assert!(!s.resolve("nope"));
    }

    #[test]
    fn edit_comment_replaces_body_and_keeps_status() {
        let mut s = Session::default();
        let id = s
            .add_comment(anchor("a.txt", Some(3)), "reviewer", "old body")
            .id
            .clone();
        assert!(s.reply(&id, "agent", "ack"));
        assert!(s.edit_comment(&id, "new body"));
        let c = s.comments.iter().find(|c| c.id == id).expect("comment");
        assert_eq!(c.body, "new body");
        assert_eq!(c.status, CommentStatus::Replied, "status is untouched");
        assert_eq!(c.replies.len(), 1, "replies are untouched");
        assert!(!s.edit_comment("nope", "x"));
    }

    #[test]
    fn unresolved_comments_filters_resolved_only() {
        let mut s = Session::default();
        let keep = s
            .add_comment(anchor("a.txt", Some(3)), "reviewer", "open")
            .id
            .clone();
        let done = s
            .add_comment(anchor("a.txt", Some(3)), "reviewer", "done")
            .id
            .clone();
        assert!(s.resolve(&done));
        let unresolved: Vec<_> = s
            .comments
            .iter()
            .filter(|c| c.status != CommentStatus::Resolved)
            .map(|c| c.id.clone())
            .collect();
        assert_eq!(unresolved, vec![keep]);
    }

    #[test]
    fn session_serializes_round_trip_with_range_anchor() {
        let mut s = Session::default();
        let mut range = anchor("a.txt", Some(3));
        range.line_end = Some(7);
        s.add_comment(range, "reviewer", "this whole block");
        s.mark_viewed("b.txt", "hash-b");
        let json = serde_json::to_string(&s).expect("serialize");
        let back: Session = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back);
        assert_eq!(back.comments[0].anchor.line_end, Some(7));
    }

    #[test]
    fn is_viewed_true_for_same_hash_false_after_change() {
        let mut s = Session::default();
        s.mark_viewed("a.txt", "hash-1");
        assert!(s.is_viewed("a.txt", "hash-1"));
        assert!(!s.is_viewed("a.txt", "hash-2"));
        assert!(!s.is_viewed("other.txt", "hash-1"));
    }

    #[test]
    fn unmark_viewed_removes_entry() {
        let mut s = Session::default();
        s.mark_viewed("a.txt", "hash-1");
        s.unmark_viewed("a.txt");
        assert!(!s.is_viewed("a.txt", "hash-1"));
    }

    /// One file, one hunk: context(1/1), deleted(2), added(2), context(3/3).
    fn hunked_model() -> DiffModel {
        DiffModel {
            files: vec![FileDiff {
                path: "src/auth.py".into(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                old_text: None,
                new_text: None,
                hunks: vec![Hunk {
                    id: HunkId("h1".into()),
                    old_start: 1,
                    old_lines: 3,
                    new_start: 1,
                    new_lines: 3,
                    context: String::new(),
                    lines: vec![
                        DiffLine::new(LineKind::Context, Some(1), Some(1), "one".into()),
                        DiffLine::new(LineKind::Deleted, Some(2), None, "two".into()),
                        DiffLine::new(LineKind::Added, None, Some(2), "TWO".into()),
                        DiffLine::new(LineKind::Context, Some(3), Some(3), "three".into()),
                    ],
                }],
                hashes: crate::model::HashCache::default(),
            }],
        }
    }

    #[test]
    fn anchor_with_matching_line_text_is_current() {
        let mut a = anchor("src/auth.py", Some(2));
        a.line_text = Some("TWO".to_owned());
        assert!(!a.is_outdated(&hunked_model()));
        // without a snapshot, a present line counts as current
        a.line_text = None;
        assert!(!a.is_outdated(&hunked_model()));
    }

    #[test]
    fn anchor_with_drifted_line_text_is_outdated() {
        let mut a = anchor("src/auth.py", Some(2));
        a.line_text = Some("old text".to_owned());
        assert!(a.is_outdated(&hunked_model()));
    }

    #[test]
    fn anchor_to_a_departed_line_is_outdated() {
        let a = anchor("src/auth.py", Some(99));
        assert!(a.is_outdated(&hunked_model()));
    }

    #[test]
    fn old_side_anchor_checks_the_old_line() {
        let mut a = anchor("src/auth.py", Some(2));
        a.on_old_side = true;
        a.line_text = Some("two".to_owned());
        assert!(!a.is_outdated(&hunked_model()));
        a.line_text = Some("TWO".to_owned());
        assert!(a.is_outdated(&hunked_model()), "old side carries 'two'");
    }

    #[test]
    fn range_anchor_judges_drift_on_its_end_line() {
        let mut a = anchor("src/auth.py", Some(1));
        a.line_end = Some(3);
        a.line_text = Some("three".to_owned());
        assert!(!a.is_outdated(&hunked_model()), "end line still matches");
        a.line_text = Some("changed".to_owned());
        assert!(a.is_outdated(&hunked_model()), "end line drifted");
    }

    #[test]
    fn file_level_anchor_is_outdated_only_when_the_file_departs() {
        assert!(!anchor("src/auth.py", None).is_outdated(&hunked_model()));
        assert!(anchor("gone.py", None).is_outdated(&hunked_model()));
    }

    #[test]
    fn reconcile_drops_departed_and_changed_keeps_matching() {
        let kept = file_diff("kept.txt", "stable content\n");
        let changed = file_diff("changed.txt", "rewritten content\n");

        let mut s = Session::default();
        s.mark_viewed("kept.txt", &kept.content_hash());
        s.mark_viewed("changed.txt", "hash-of-old-content");
        s.mark_viewed("departed.txt", "whatever");
        s.add_comment(
            anchor("departed.txt", Some(3)),
            "reviewer",
            "still relevant",
        );

        s.reconcile(&model(vec![kept, changed]));

        assert!(s.viewed.contains_key("kept.txt"));
        assert!(!s.viewed.contains_key("changed.txt"));
        assert!(!s.viewed.contains_key("departed.txt"));
        // comments survive reconciliation untouched
        assert_eq!(s.comments.len(), 1);
    }
}
