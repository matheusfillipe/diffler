//! Review session: comments and per-file viewed marks, reconciled against
//! fresh diff models. Persistence lives in `store`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::DiffModel;
use crate::walkthrough::Walkthrough;

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
    /// The rows this anchor covers, on whichever side it names: `line`
    /// through `line_end` (or just `line` for a point anchor). `None` for a
    /// file-level anchor with no line at all.
    pub fn span(&self) -> Option<(u32, u32)> {
        self.line.map(|line| (line, self.line_end.unwrap_or(line)))
    }

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
    /// The forge's review-thread handle, where the forge has one: what
    /// thread resolution posts against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub anchor: Anchor,
    /// The agent's own name for this comment: a walkthrough stop's title,
    /// set when it is published.
    #[serde(default)]
    pub title: Option<String>,
    /// The anchor an agent wrote (`path#symbol`, `path:start-end`, `path`),
    /// kept so the worker can resolve it again after the code moves.
    #[serde(default)]
    pub anchor_ref: Option<String>,
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
    /// The walkthrough this session is, when its source is
    /// `ReviewSource::Walkthrough`. Every comment in `comments` above is this
    /// walkthrough's: its stops, their notes, and every human reply made on
    /// it. `None` for every other source.
    #[serde(default)]
    pub walkthrough: Option<Walkthrough>,
    /// Walkthrough stops (comment ids) the reader has marked read. Pruned to
    /// the current walkthrough's `stops` on every change, so a stop id from a
    /// superseded revision never lingers.
    #[serde(default)]
    pub seen_stops: BTreeSet<String>,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Unix milliseconds, for a stamp that also has to order two things made in
/// the same second. A stamp in seconds is smaller than any of these, so the
/// two sort together and the older one still reads as older.
pub fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Session {
    pub fn add_comment(&mut self, anchor: Anchor, author: &str, body: &str) -> &Comment {
        self.comments.push(Comment {
            remote_id: None,
            thread_id: None,
            id: uuid::Uuid::new_v4().to_string(),
            author: author.to_owned(),
            anchor,
            title: None,
            anchor_ref: None,
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

    /// Set this session's walkthrough, replacing whatever it held before,
    /// and prune seen marks for stops it no longer carries.
    pub fn set_walkthrough(&mut self, walkthrough: Walkthrough) {
        self.walkthrough = Some(walkthrough);
        self.prune_seen_stops();
    }

    /// Remove one stop and its notes, keeping the walkthrough and its other
    /// stops. `false` when there is no walkthrough or `index` is out of range.
    pub fn delete_stop(&mut self, index: usize) -> bool {
        let Some(walkthrough) = self.walkthrough.as_ref() else {
            return false;
        };
        let Some(stop_id) = walkthrough.stops.get(index).cloned() else {
            return false;
        };
        let mut remove: BTreeSet<String> = walkthrough
            .notes_by_stop(&self.comments)
            .get(index)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        remove.insert(stop_id);
        let Some(walkthrough) = self.walkthrough.as_mut() else {
            return false;
        };
        walkthrough.stops.remove(index);
        self.comments.retain(|c| !remove.contains(&c.id));
        self.prune_seen_stops();
        true
    }

    /// Drop a seen mark for a stop the walkthrough no longer carries: a stop
    /// id from a superseded revision would otherwise linger in `seen_stops`
    /// forever.
    fn prune_seen_stops(&mut self) {
        let stops: BTreeSet<&str> = self
            .walkthrough
            .iter()
            .flat_map(|w| w.stops.iter().map(String::as_str))
            .collect();
        self.seen_stops.retain(|id| stops.contains(id.as_str()));
    }

    /// Mark a walkthrough stop read.
    pub fn mark_stop_seen(&mut self, id: &str) {
        self.seen_stops.insert(id.to_owned());
    }

    pub fn unmark_stop_seen(&mut self, id: &str) {
        self.seen_stops.remove(id);
    }

    pub fn is_stop_seen(&self, id: &str) -> bool {
        self.seen_stops.contains(id)
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
        self.mark_replied(comment_id)
    }

    pub fn comment(&self, id: &str) -> Option<&Comment> {
        self.comments.iter().find(|comment| comment.id == id)
    }

    /// Flag a comment as addressed without writing anything into the thread.
    /// An agent that already answered has said its piece; a second summary of
    /// it is noise in the card.
    pub fn mark_replied(&mut self, comment_id: &str) -> bool {
        let Some(comment) = self.comment_mut(comment_id) else {
            return false;
        };
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

    /// Drop every viewed mark, sending all files back to the review pile.
    pub fn clear_viewed(&mut self) {
        self.viewed.clear();
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

    /// A review file written before the walkthrough field existed still
    /// loads, with none, so an old `.diffler/reviews/*.json` keeps working
    /// after an upgrade.
    #[test]
    fn a_session_with_the_old_boards_key_and_no_walkthrough_key_loads_with_none() {
        let json = r#"{"comments":[],"viewed":{},"boards":[]}"#;
        let s: Session = serde_json::from_str(json).expect("deserialize");
        assert!(s.walkthrough.is_none());
    }

    /// A dedicated walkthrough session deserializes its walkthrough directly;
    /// an older `comments` field on the object (the pre-source owned-id list)
    /// is simply unknown and ignored.
    #[test]
    fn a_walkthrough_session_deserializes_its_walkthrough_and_ignores_a_stale_comments_field() {
        let json = r#"{"comments":[],"viewed":{},"walkthrough":{"id":"w1","title":"tour","author":"agent","at":1,"stops":["stop-0"],"comments":["stop-0"]}}"#;
        let s: Session = serde_json::from_str(json).expect("deserialize");
        assert_eq!(s.walkthrough.expect("walkthrough").id, "w1");
    }

    /// A review file written before seen marks existed carries no
    /// `seen_stops` key at all, and still has to load.
    #[test]
    fn a_session_with_no_seen_stops_key_loads_empty() {
        let json = r#"{"comments":[],"viewed":{}}"#;
        let s: Session = serde_json::from_str(json).expect("deserialize");
        assert!(s.seen_stops.is_empty());
    }

    #[test]
    fn mark_and_unmark_stop_seen_round_trip() {
        let mut s = Session::default();
        assert!(!s.is_stop_seen("stop-0"));
        s.mark_stop_seen("stop-0");
        assert!(s.is_stop_seen("stop-0"));
        s.unmark_stop_seen("stop-0");
        assert!(!s.is_stop_seen("stop-0"));
    }

    fn walkthrough(id: &str, stops: &[&str]) -> Walkthrough {
        Walkthrough {
            id: id.to_owned(),
            title: "tour".to_owned(),
            author: "agent".to_owned(),
            at: 1,
            stops: stops.iter().map(|s| (*s).to_owned()).collect(),
            skipped: None,
            summary: None,
            rev: None,
            about: crate::source::ReviewSource::WorkingTree,
        }
    }

    /// Setting a walkthrough drops a seen mark for a stop it no longer
    /// carries, so a superseded stop id never lingers.
    #[test]
    fn set_walkthrough_prunes_seen_marks_for_dropped_stops() {
        let mut s = Session::default();
        s.mark_stop_seen("stop-0");
        s.mark_stop_seen("stop-1");
        s.set_walkthrough(walkthrough("w1", &["stop-1"]));
        assert!(!s.is_stop_seen("stop-0"), "stop-0 no longer exists");
        assert!(s.is_stop_seen("stop-1"), "stop-1 survives the revision");
    }

    /// Setting a walkthrough replaces whatever this session held before, in
    /// place: there is only ever one.
    #[test]
    fn set_walkthrough_replaces_whatever_was_there() {
        let mut s = Session::default();
        s.set_walkthrough(walkthrough("w1", &["a"]));
        s.set_walkthrough(Walkthrough {
            title: "revised".to_owned(),
            at: 2,
            ..walkthrough("w1", &["a"])
        });
        assert_eq!(s.walkthrough.expect("walkthrough").title, "revised");
    }

    fn agent_comment(id: &str, file: &str, line: u32, title: Option<&str>) -> Comment {
        Comment {
            id: id.to_owned(),
            author: "agent".to_owned(),
            remote_id: None,
            thread_id: None,
            anchor: anchor(file, Some(line)),
            title: title.map(str::to_owned),
            anchor_ref: Some(format!("{file}:{line}")),
            body: "why".to_owned(),
            status: CommentStatus::Open,
            replies: Vec::new(),
            at: 1,
        }
    }

    fn human_comment(id: &str, file: &str, line: u32) -> Comment {
        Comment {
            author: "reviewer".to_owned(),
            title: None,
            anchor_ref: None,
            ..agent_comment(id, file, line, None)
        }
    }

    /// Deleting one stop drops its primary and every note anchored in its
    /// region, keeps the rest of the walkthrough, and never touches a human
    /// comment in the same spot.
    #[test]
    fn delete_stop_removes_its_primary_and_notes_but_keeps_a_human_comment_there() {
        let mut s = Session::default();
        s.comments
            .push(agent_comment("stop-0", "a.txt", 1, Some("first")));
        s.comments.push(agent_comment("note-0", "a.txt", 1, None));
        s.comments.push(human_comment("human-0", "a.txt", 1));
        s.comments
            .push(agent_comment("stop-1", "b.txt", 1, Some("second")));
        s.set_walkthrough(walkthrough("w1", &["stop-0", "stop-1"]));

        assert!(s.delete_stop(0));

        let walkthrough = s.walkthrough.as_ref().expect("the walkthrough is kept");
        assert_eq!(walkthrough.stops, ["stop-1".to_owned()]);
        let ids: Vec<&str> = s.comments.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            ["human-0", "stop-1"],
            "the human comment in stop-0's region survives"
        );
    }

    #[test]
    fn delete_stop_is_false_out_of_range_or_without_a_walkthrough() {
        let mut s = Session::default();
        assert!(!s.delete_stop(0), "no walkthrough at all");

        s.comments
            .push(agent_comment("stop-0", "a.txt", 1, Some("first")));
        s.set_walkthrough(walkthrough("w1", &["stop-0"]));
        assert!(!s.delete_stop(5), "past the end");
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

    #[test]
    fn clear_viewed_drops_every_mark() {
        let mut s = Session::default();
        s.mark_viewed("a.txt", "hash-1");
        s.mark_viewed("b.txt", "hash-2");
        s.clear_viewed();
        assert!(!s.is_viewed("a.txt", "hash-1"));
        assert!(!s.is_viewed("b.txt", "hash-2"));
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
