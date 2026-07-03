//! The PR review loop against the forge: pull the PR's line comments into
//! the local session (the same review UI everywhere) and push local comments
//! and replies back out.

use diffler_core::session::{Anchor, Comment, Reply};
use diffler_core::source::ReviewSource;

use super::App;
use crate::ci::PrComment;

/// One queued outbound post; results return as events and stamp `remote_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrPost {
    Comment {
        number: u64,
        head_oid: String,
        comment_id: String,
        path: String,
        line: u32,
        body: String,
    },
    Reply {
        number: u64,
        comment_id: String,
        reply_index: usize,
        parent_remote_id: String,
        body: String,
    },
}

impl App {
    pub(crate) fn on_pr_comments_event(
        &mut self,
        number: u64,
        remote: &[PrComment],
    ) -> super::Flow {
        self.sync_pr_comments(number, remote);
        super::Flow::Continue
    }

    pub(crate) fn on_pr_posted_event(
        &mut self,
        post: &PrPost,
        result: Result<PrComment, String>,
    ) -> super::Flow {
        self.on_pr_posted(post, result);
        super::Flow::Continue
    }

    /// Replace the PR session's forge-synced comments with the fresh listing,
    /// keeping local ones (no `remote_id`) untouched. Thread roots become
    /// comments; replies attach to their root by forge id.
    pub(crate) fn sync_pr_comments(&mut self, number: u64, remote: &[PrComment]) {
        let source = ReviewSource::pr(number);
        let model = self.source_model(&source);
        let mut roots: Vec<Comment> = Vec::new();
        for item in remote.iter().filter(|c| c.reply_to.is_none()) {
            let line_text = item.line.and_then(|line| {
                let file = model.files.iter().find(|f| f.path == item.path)?;
                file.hunks
                    .iter()
                    .flat_map(|h| &h.lines)
                    .find(|l| {
                        if item.new_side {
                            l.new_no == Some(line)
                        } else {
                            l.old_no == Some(line)
                        }
                    })
                    .map(|l| l.text.clone())
            });
            roots.push(Comment {
                id: format!("remote-{}", item.id),
                remote_id: Some(item.id.clone()),
                author: item.author.clone(),
                anchor: Anchor {
                    file: item.path.clone(),
                    line: item.line,
                    line_end: None,
                    on_old_side: !item.new_side,
                    line_text,
                },
                body: item.body.clone(),
                status: diffler_core::session::CommentStatus::Open,
                replies: Vec::new(),
                at: item.at,
            });
        }
        for item in remote.iter().filter(|c| c.reply_to.is_some()) {
            if let Some(root) = roots.iter_mut().find(|c| c.remote_id == item.reply_to) {
                root.replies.push(Reply {
                    remote_id: Some(item.id.clone()),
                    author: item.author.clone(),
                    body: item.body.clone(),
                    at: item.at,
                });
            }
        }
        let session = self.review.session_for_mut(&source);
        session.comments.retain(|c| c.remote_id.is_none());
        session.comments.extend(roots);
        session.comments.sort_by_key(|c| c.at);
        if let Err(err) = self.review.save_for(&source) {
            self.error(err.to_string());
        }
        if let Some(diff) = self.diff.as_mut() {
            diff.invalidate();
        }
    }

    /// Queue forge posts for every unsent comment and reply in the active PR
    /// session. Called after any session change; the inflight set keeps a
    /// slow forge from double-posting.
    pub(crate) fn queue_pr_posts(&mut self) {
        let ReviewSource::Pr { number } = self.active_review_source() else {
            return;
        };
        let Some((_, head)) = self.pr_ranges.get(&number).cloned() else {
            return;
        };
        let session = self.review.session_for(&ReviewSource::pr(number));
        let mut posts = Vec::new();
        for comment in &session.comments {
            match (&comment.remote_id, comment.anchor.line) {
                (None, Some(line)) if !comment.anchor.on_old_side => {
                    posts.push(PrPost::Comment {
                        number,
                        head_oid: head.clone(),
                        comment_id: comment.id.clone(),
                        path: comment.anchor.file.clone(),
                        line: comment.anchor.line_end.unwrap_or(line),
                        body: comment.body.clone(),
                    });
                }
                (Some(parent), _) => {
                    for (reply_index, reply) in comment.replies.iter().enumerate() {
                        if reply.remote_id.is_none() {
                            posts.push(PrPost::Reply {
                                number,
                                comment_id: comment.id.clone(),
                                reply_index,
                                parent_remote_id: parent.clone(),
                                body: reply.body.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        for post in posts {
            let key = post_key(&post);
            if self.pr_posts_inflight.insert(key) {
                self.pending_pr_posts.push(post);
            }
        }
    }

    /// A completed post: stamp the forge id onto the local comment or reply
    /// so it stops re-queueing and future syncs recognize it.
    pub(crate) fn on_pr_posted(&mut self, post: &PrPost, result: Result<PrComment, String>) {
        self.pr_posts_inflight.remove(&post_key(post));
        let number = match post {
            PrPost::Comment { number, .. } | PrPost::Reply { number, .. } => *number,
        };
        let source = ReviewSource::pr(number);
        match result {
            Ok(remote) => {
                let session = self.review.session_for_mut(&source);
                match post {
                    PrPost::Comment { comment_id, .. } => {
                        if let Some(c) = session.comments.iter_mut().find(|c| c.id == *comment_id) {
                            c.remote_id = Some(remote.id);
                        }
                    }
                    PrPost::Reply {
                        comment_id,
                        reply_index,
                        ..
                    } => {
                        if let Some(r) = session
                            .comments
                            .iter_mut()
                            .find(|c| c.id == *comment_id)
                            .and_then(|c| c.replies.get_mut(*reply_index))
                        {
                            r.remote_id = Some(remote.id);
                        }
                    }
                }
                if let Err(err) = self.review.save_for(&source) {
                    self.error(err.to_string());
                }
            }
            Err(err) => self.error(format!("posting to the PR failed: {err}")),
        }
    }
}

fn post_key(post: &PrPost) -> String {
    match post {
        PrPost::Comment { comment_id, .. } => format!("c-{comment_id}"),
        PrPost::Reply {
            comment_id,
            reply_index,
            ..
        } => format!("r-{comment_id}-{reply_index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    #[test]
    fn sync_maps_threads_and_posting_stamps_remote_ids() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        app.pr_ranges.insert(7, (head.clone(), head.clone()));

        app.sync_pr_comments(
            7,
            &[
                PrComment {
                    id: "100".into(),
                    path: "app.txt".into(),
                    line: Some(2),
                    new_side: true,
                    body: "remote root".into(),
                    author: "alice".into(),
                    reply_to: None,
                    at: 10,
                },
                PrComment {
                    id: "101".into(),
                    path: "app.txt".into(),
                    line: Some(2),
                    new_side: true,
                    body: "remote reply".into(),
                    author: "bob".into(),
                    reply_to: Some("100".into()),
                    at: 11,
                },
            ],
        );
        let source = ReviewSource::pr(7);
        {
            let session = app.review.session_for(&source);
            assert_eq!(session.comments.len(), 1);
            assert_eq!(session.comments[0].remote_id.as_deref(), Some("100"));
            assert_eq!(session.comments[0].replies.len(), 1);
            assert_eq!(session.comments[0].author, "alice");
        }

        // a local comment on the PR source queues exactly one outbound post
        let anchor = Anchor {
            file: "app.txt".into(),
            line: Some(3),
            line_end: None,
            on_old_side: false,
            line_text: None,
        };
        let local_id = app
            .review
            .session_for_mut(&source)
            .add_comment("me", anchor, "needs work")
            .id
            .clone();
        app.open_pr_diff(7, &head, &head);
        assert!(app.diff.is_some(), "the PR diff view opened");
        app.queue_pr_posts();
        assert_eq!(app.pending_pr_posts.len(), 1);
        app.queue_pr_posts();
        assert_eq!(
            app.pending_pr_posts.len(),
            1,
            "inflight set blocks re-queue"
        );

        let post = app.pending_pr_posts.remove(0);
        app.on_pr_posted(
            &post,
            Ok(PrComment {
                id: "200".into(),
                path: "app.txt".into(),
                line: Some(3),
                new_side: true,
                body: "needs work".into(),
                author: "me".into(),
                reply_to: None,
                at: 12,
            }),
        );
        let session = app.review.session_for(&source);
        let mine = session
            .comments
            .iter()
            .find(|c| c.id == local_id)
            .expect("kept");
        assert_eq!(mine.remote_id.as_deref(), Some("200"));

        // the next sync keeps the now-remote local comment out of the purge
        app.sync_pr_comments(7, &[]);
        let session = app.review.session_for(&source);
        assert!(
            session.comments.iter().all(|c| c.id != local_id),
            "forge-synced comments follow the forge listing"
        );
    }
}
