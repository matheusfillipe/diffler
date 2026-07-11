//! The PR review loop against the forge: pull the PR's line comments into
//! the local session (the same review UI everywhere) and push local comments
//! and replies back out.

use diffler_core::session::{Anchor, Comment, CommentStatus, Reply};
use diffler_core::source::ReviewSource;

use super::App;
use crate::ci::{PrComment, ReviewVerdict};

/// One queued outbound post; results return as events and stamp `remote_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrPost {
    /// Every pending comment as one review — a single forge notification.
    Review {
        review: crate::ci::NewPrReview,
        /// Local ids the review covers; forge-owned copies replace them on
        /// the resync a successful submit triggers.
        comment_ids: Vec<String>,
    },
    Reply {
        number: u64,
        comment_id: String,
        reply_index: usize,
        parent_remote_id: String,
        body: String,
    },
    /// Resolve or unresolve a forge thread; the local status flips
    /// optimistically and reverts if the post fails.
    Resolve {
        number: u64,
        comment_id: String,
        thread_id: String,
        resolved: bool,
    },
    /// Rewrite one of our own forge comments (already edited locally).
    Edit {
        number: u64,
        comment_id: String,
        remote_id: String,
        body: String,
    },
    /// Delete one of our own forge comments; the local copy goes only after
    /// the forge confirms.
    Delete {
        number: u64,
        comment_id: String,
        remote_id: String,
    },
}

impl App {
    /// Open the forge's PR list; the entries arrive as an event.
    pub(crate) fn open_prs(&mut self) {
        if self.ci_remotes().is_empty() {
            self.info("no forge detected for this repo");
            return;
        }
        self.prs_cursor = 0;
        self.push_screen(super::Screen::Prs);
        self.pending_ci = Some(super::CiRequest::Prs);
    }

    pub(crate) fn on_prs_event(&mut self, prs: Vec<crate::ci::PullRequest>) -> super::Flow {
        self.prs = prs;
        self.prs_cursor = self.prs_cursor.min(self.prs.len().saturating_sub(1));
        super::Flow::Continue
    }

    /// The PR list from keymap actions: list motions, Enter reviews the PR
    /// (its branch never needs to be checked out), `b` checks the branch out.
    pub(crate) fn dispatch_prs(&mut self, action: crate::keymap::Action) {
        use crate::keymap::Action;
        let last = self.prs.len().saturating_sub(1);
        match action {
            Action::MoveDown => self.prs_cursor = (self.prs_cursor + 1).min(last),
            Action::MoveUp => self.prs_cursor = self.prs_cursor.saturating_sub(1),
            Action::GoTop => self.prs_cursor = 0,
            Action::GoBottom => self.prs_cursor = last,
            Action::HalfPageDown => {
                self.prs_cursor = (self.prs_cursor + super::page_step(0, false)).min(last);
            }
            Action::FullPageDown => {
                self.prs_cursor = (self.prs_cursor + super::page_step(0, true)).min(last);
            }
            Action::HalfPageUp => {
                self.prs_cursor = self.prs_cursor.saturating_sub(super::page_step(0, false));
            }
            Action::FullPageUp => {
                self.prs_cursor = self.prs_cursor.saturating_sub(super::page_step(0, true));
            }
            Action::Refresh => self.pending_ci = Some(super::CiRequest::Prs),
            Action::Open => {
                if let Some(pr) = self.prs.get(self.prs_cursor).cloned() {
                    self.open_pr_review_for(pr);
                }
            }
            Action::BranchCheckout => self.checkout_selected_pr(),
            _ => {}
        }
    }

    /// Marks the git ops whose completion may consume the `pending_pr_*`
    /// continuation slots: several ops can be in flight at once, so
    /// `git_finished` must not route an unrelated op's result into them.
    pub(crate) const PR_FETCH_PREFIX: &'static str = "fetch PR #";

    pub(crate) fn pr_fetch_label(number: u64) -> String {
        format!("{}{number}", Self::PR_FETCH_PREFIX)
    }

    /// Fetch the PR's head into a local branch and switch to it. GitHub's CLI
    /// does both (and handles forks); other forges fetch the pull ref into a
    /// branch named after the PR head and switch when the fetch lands.
    pub(crate) fn checkout_selected_pr(&mut self) {
        let Some(pr) = self.prs.get(self.prs_cursor).cloned() else {
            return;
        };
        let github = self
            .ci_remotes()
            .first()
            .is_some_and(|r| matches!(r.detected.kind, crate::ci::ProviderKind::GitHub));
        if github {
            self.pending_git = Some(super::GitOp {
                label: format!("checkout PR #{}", pr.number),
                argv: vec![
                    "gh".to_owned(),
                    "pr".to_owned(),
                    "checkout".to_owned(),
                    pr.number.to_string(),
                ],
            });
            return;
        }
        let remote = self
            .ci_remotes()
            .first()
            .map_or_else(|| "origin".to_owned(), |r| r.name.clone());
        self.pending_pr_switch = Some(pr.head_ref.clone());
        self.pending_git = Some(super::GitOp {
            label: Self::pr_fetch_label(pr.number),
            argv: vec![
                "git".to_owned(),
                "fetch".to_owned(),
                remote,
                format!("refs/pull/{}/head:refs/heads/{}", pr.number, pr.head_ref),
            ],
        });
    }

    pub(crate) fn on_pr_comments_event(
        &mut self,
        number: u64,
        remote: &[PrComment],
        pr: Option<crate::ci::PullRequest>,
    ) -> super::Flow {
        if let Some(pr) = pr {
            self.on_pr_head_seen(&pr);
        }
        self.sync_pr_comments(number, remote);
        super::Flow::Continue
    }

    pub(crate) fn on_pr_posted_event(
        &mut self,
        post: &PrPost,
        result: Result<Option<PrComment>, String>,
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
                model
                    .find_line(&item.path, line, !item.new_side)
                    .map(|l| l.text.clone())
            });
            let status = if item.resolved {
                CommentStatus::Resolved
            } else {
                CommentStatus::Open
            };
            roots.push(Comment {
                id: format!("remote-{}", item.id),
                remote_id: Some(item.id.clone()),
                thread_id: item.thread_id.clone(),
                author: item.author.clone(),
                anchor: Anchor {
                    file: item.path.clone(),
                    // a multi-line comment anchors its range start..end
                    line: item.start_line.or(item.line),
                    line_end: item.start_line.and(item.line),
                    on_old_side: !item.new_side,
                    line_text,
                },
                body: item.body.clone(),
                status,
                replies: Vec::new(),
                at: item.at,
            });
        }
        let inflight = self.pr_posts_inflight.clone();
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
        // a re-imported root must not clobber local state: unsent replies and
        // the locally set status live only here, not on the forge
        for root in &mut roots {
            let Some(prior) = session
                .comments
                .iter()
                .find(|c| c.remote_id == root.remote_id)
            else {
                continue;
            };
            // the forge's resolution is authoritative; other statuses
            // (Replied) are local workflow state and survive the sync. An
            // optimistic flip (either direction) whose post is still in
            // flight also survives — the forge just hasn't heard yet
            let flip_inflight = inflight
                .iter()
                .any(|key| key.starts_with(&resolve_key_prefix(&root.id)));
            root.status = if flip_inflight {
                prior.status
            } else {
                match (root.status, prior.status) {
                    (CommentStatus::Resolved, _) => CommentStatus::Resolved,
                    (_, CommentStatus::Resolved) => CommentStatus::Open,
                    (_, prior) => prior,
                }
            };
            // same for a body rewrite the forge hasn't acknowledged yet
            let edit_inflight = inflight
                .iter()
                .any(|key| key.starts_with(&edit_key_prefix(&root.id)));
            if edit_inflight {
                root.body.clone_from(&prior.body);
            }
            root.replies.extend(
                prior
                    .replies
                    .iter()
                    .filter(|r| r.remote_id.is_none())
                    .cloned(),
            );
        }
        let mut merged: Vec<Comment> = session
            .comments
            .iter()
            .filter(|c| c.remote_id.is_none())
            .cloned()
            .collect();
        merged.extend(roots);
        merged.sort_by_key(|c| c.at);
        // a poll that changed nothing must not dirty the store: the write wakes
        // the watcher, which refreshes, which redraws — a self-sustaining storm
        if merged == session.comments {
            return;
        }
        session.comments = merged;
        if let Err(err) = self.review.save_for(&source) {
            self.error(err.to_string());
        }
        if let Some(diff) = self.diff.as_mut() {
            diff.invalidate();
        }
    }

    /// `S`: start the review submit — pick the verdict first, then an
    /// optional summary body, then everything pending posts as one review.
    pub(crate) fn submit_pr_review(&mut self) {
        let ReviewSource::Pr { number } = self.active_review_source() else {
            self.info("not reviewing a PR — nothing to submit");
            return;
        };
        if !self.pr_ranges.contains_key(&number) {
            return;
        }
        self.modal = Some(super::Modal::ReviewVerdict { number });
    }

    /// The verdict is chosen; ask for the review's optional summary body.
    pub(crate) fn pr_review_verdict_chosen(&mut self, number: u64, verdict: ReviewVerdict) {
        self.open_input(
            "Review summary (optional)".to_owned(),
            String::new(),
            super::InputOp::ReviewBody { number, verdict },
        );
    }

    /// Queue everything pending in the PR review as one forge review with
    /// `verdict` and `body` (plus individual replies to existing threads),
    /// so the forge sends a single notification.
    pub(crate) fn queue_pr_review(&mut self, number: u64, verdict: ReviewVerdict, body: &str) {
        let Some((_, head)) = self.pr_ranges.get(&number).cloned() else {
            return;
        };
        let session = self.review.session_for(&ReviewSource::pr(number));
        let mut review_comments = Vec::new();
        let mut comment_ids = Vec::new();
        let mut replies = Vec::new();
        for comment in &session.comments {
            match (&comment.remote_id, comment.anchor.line) {
                (None, Some(line)) => {
                    // a range anchor posts as a real multi-line comment
                    let (start_line, line) = match comment.anchor.line_end {
                        Some(end) if end != line => (Some(line), end),
                        _ => (None, line),
                    };
                    // unsent replies under an unsent comment ride along in the
                    // review at the same anchor: a flattened thread beats a
                    // lost reply
                    let bodies = std::iter::once(comment.body.clone())
                        .chain(comment.replies.iter().map(|r| r.body.clone()));
                    for body in bodies {
                        review_comments.push(crate::ci::NewPrComment {
                            number,
                            head_oid: head.clone(),
                            path: comment.anchor.file.clone(),
                            line,
                            start_line,
                            new_side: !comment.anchor.on_old_side,
                            body,
                        });
                    }
                    comment_ids.push(comment.id.clone());
                }
                (Some(parent), _) => {
                    for (reply_index, reply) in comment.replies.iter().enumerate() {
                        if reply.remote_id.is_none() {
                            replies.push(PrPost::Reply {
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
        // reviews carry line comments only; a line-less (whole-file) anchor
        // has no review slot on the forge
        let file_level = session
            .comments
            .iter()
            .filter(|c| c.remote_id.is_none() && c.anchor.line.is_none())
            .count();
        // GitHub requires a summary for request-changes; a bare COMMENT
        // review with nothing to say is an empty notification
        if verdict == ReviewVerdict::RequestChanges && body.is_empty() {
            self.info("request changes needs a summary");
            return;
        }
        if verdict == ReviewVerdict::Comment
            && review_comments.is_empty()
            && body.is_empty()
            && replies.is_empty()
        {
            self.info("nothing pending to submit");
            return;
        }
        let total = review_comments.len() + replies.len();
        let mut posts = replies;
        if !review_comments.is_empty() || verdict != ReviewVerdict::Comment || !body.is_empty() {
            posts.push(PrPost::Review {
                review: crate::ci::NewPrReview {
                    number,
                    head_oid: head,
                    verdict,
                    body: body.to_owned(),
                    comments: review_comments,
                },
                comment_ids,
            });
        }
        for post in posts {
            self.queue_pr_post(post);
        }
        let label = match verdict {
            ReviewVerdict::Approve => "approving",
            ReviewVerdict::RequestChanges => "requesting changes",
            ReviewVerdict::Comment => "commenting",
        };
        let mut message = format!("submitting review — {label} ({total} comments)…");
        if file_level > 0 {
            use std::fmt::Write as _;
            let _ = write!(message, " {file_level} whole-file comment(s) stay local");
        }
        self.info(message);
    }

    /// Queue one outbound post unless an identical one is already in flight.
    fn queue_pr_post(&mut self, post: PrPost) {
        if self.pr_posts_inflight.insert(post_key(&post)) {
            self.pending_pr_posts.push(post);
        }
    }

    /// Drop every queued post, freeing the dedup keys — a key left behind
    /// would block that comment's posts for the rest of the session.
    pub fn drop_pending_pr_posts(&mut self) {
        for post in self.pending_pr_posts.drain(..) {
            self.pr_posts_inflight.remove(&post_key(&post));
        }
    }

    /// A completed post. A reply stamps its forge id; a submitted review
    /// hands its comments over to the forge — the local copies go away and
    /// the immediate resync brings back the canonical ones.
    pub(crate) fn on_pr_posted(
        &mut self,
        post: &PrPost,
        result: Result<Option<PrComment>, String>,
    ) {
        self.pr_posts_inflight.remove(&post_key(post));
        let number = match post {
            PrPost::Review { review, .. } => review.number,
            PrPost::Reply { number, .. }
            | PrPost::Resolve { number, .. }
            | PrPost::Edit { number, .. }
            | PrPost::Delete { number, .. } => *number,
        };
        let source = ReviewSource::pr(number);
        match result {
            Ok(remote) => {
                let session = self.review.session_for_mut(&source);
                match post {
                    PrPost::Review { comment_ids, .. } => {
                        session.comments.retain(|c| !comment_ids.contains(&c.id));
                        self.pending_ci = Some(super::CiRequest::PrComments(number));
                    }
                    PrPost::Reply {
                        comment_id, body, ..
                    } => {
                        // a resync can reshape the replies vec while the post
                        // is in flight, so the queue-time index is unsafe:
                        // stamp the matching unsent reply instead
                        if let Some(r) = session
                            .comments
                            .iter_mut()
                            .find(|c| c.id == *comment_id)
                            .and_then(|c| {
                                c.replies
                                    .iter_mut()
                                    .find(|r| r.remote_id.is_none() && r.body == *body)
                            })
                        {
                            r.remote_id = remote.map(|c| c.id);
                        }
                    }
                    // resolve was applied optimistically; edit already
                    // landed locally — the next sync confirms both
                    PrPost::Resolve { .. } | PrPost::Edit { .. } => {}
                    PrPost::Delete { comment_id, .. } => {
                        session.comments.retain(|c| c.id != *comment_id);
                        self.pending_ci = Some(super::CiRequest::PrComments(number));
                    }
                }
                if let Err(err) = self.review.save_for(&source) {
                    self.error(err.to_string());
                }
                if let Some(diff) = self.diff.as_mut() {
                    diff.invalidate();
                }
            }
            Err(err) => {
                // an optimistic resolve that the forge refused rolls back
                if let PrPost::Resolve {
                    comment_id,
                    resolved,
                    ..
                } = post
                {
                    let session = self.review.session_for_mut(&source);
                    if let Some(comment) = session.comments.iter_mut().find(|c| c.id == *comment_id)
                    {
                        comment.status = if *resolved {
                            CommentStatus::Open
                        } else {
                            CommentStatus::Resolved
                        };
                    }
                    if let Some(diff) = self.diff.as_mut() {
                        diff.invalidate();
                    }
                }
                self.error(format!("posting to the PR failed: {err}"));
            }
        }
    }

    /// The forge's current view of the PR arrived with a comment sync: a
    /// moved head means someone (force-)pushed while the review is open, so
    /// the diff and the post target both refresh.
    pub(crate) fn on_pr_head_seen(&mut self, pr: &crate::ci::PullRequest) {
        let moved = self
            .pr_ranges
            .get(&pr.number)
            .is_some_and(|(_, head)| *head != pr.head_oid);
        if !moved || self.pending_pr_open.is_some() {
            return;
        }
        self.info(format!(
            "PR #{} head moved — refreshing the diff",
            pr.number
        ));
        self.open_pr_review_for(pr.clone());
    }

    /// Push a local edit of a forge-owned comment out to the forge; local
    /// comments and non-PR sources are already done.
    pub(crate) fn queue_pr_comment_edit(
        &mut self,
        source: &ReviewSource,
        comment_id: &str,
        body: &str,
    ) {
        let ReviewSource::Pr { number } = source else {
            return;
        };
        let remote = self
            .review
            .session_for(source)
            .comments
            .iter()
            .find(|c| c.id == comment_id)
            .and_then(|c| c.remote_id.clone());
        let Some(remote_id) = remote else {
            return;
        };
        let post = PrPost::Edit {
            number: *number,
            comment_id: comment_id.to_owned(),
            remote_id,
            body: body.to_owned(),
        };
        self.queue_pr_post(post);
    }

    /// Queue a forge-side delete; the local copy stays until the forge
    /// confirms so a rejection loses nothing.
    pub(crate) fn queue_pr_comment_delete(
        &mut self,
        number: u64,
        comment_id: &str,
        remote_id: &str,
    ) {
        let post = PrPost::Delete {
            number,
            comment_id: comment_id.to_owned(),
            remote_id: remote_id.to_owned(),
        };
        self.queue_pr_post(post);
    }

    /// Queue a forge thread-resolution toggle. `false` when the comment has
    /// no thread handle yet (not synced, or a forge without threads) — the
    /// caller must not flip anything locally then.
    pub(crate) fn queue_pr_resolve(
        &mut self,
        number: u64,
        comment_id: &str,
        resolved: bool,
    ) -> bool {
        let thread = self
            .review
            .session_for(&ReviewSource::pr(number))
            .comments
            .iter()
            .find(|c| c.id == comment_id)
            .and_then(|c| c.thread_id.clone());
        let Some(thread_id) = thread else {
            self.info("no forge thread for this comment yet — sync pending");
            return false;
        };
        let post = PrPost::Resolve {
            number,
            comment_id: comment_id.to_owned(),
            thread_id,
            resolved,
        };
        self.queue_pr_post(post);
        true
    }
}

/// The inflight-dedup key. Resolve carries its direction so a quick toggle
/// back is not swallowed; an edit carries a body hash so a follow-up edit
/// with new text still posts.
fn post_key(post: &PrPost) -> String {
    match post {
        PrPost::Review { review, .. } => format!("review-{}", review.number),
        PrPost::Reply {
            comment_id,
            reply_index,
            ..
        } => format!("r-{comment_id}-{reply_index}"),
        PrPost::Resolve {
            comment_id,
            resolved,
            ..
        } => format!("res-{comment_id}-{resolved}"),
        PrPost::Edit {
            comment_id, body, ..
        } => {
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            body.hash(&mut hasher);
            format!("e-{comment_id}-{:x}", hasher.finish())
        }
        PrPost::Delete { comment_id, .. } => format!("d-{comment_id}"),
    }
}

/// Prefix of a resolve toggle's inflight key for `comment_id`, regardless of
/// direction — an in-flight check that doesn't care which way it flipped
/// matches on this instead of a direction-specific [`post_key`].
fn resolve_key_prefix(comment_id: &str) -> String {
    format!("res-{comment_id}-")
}

/// Prefix of an edit's inflight key for `comment_id`, regardless of the body
/// hash [`post_key`] mixes in.
fn edit_key_prefix(comment_id: &str) -> String {
    format!("e-{comment_id}-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    #[test]
    fn reviewing_a_listed_pr_never_needs_a_checkout() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.prs = vec![crate::ci::PullRequest {
            number: 9,
            title: "remote only".into(),
            url: None,
            base_ref: "main".into(),
            head_ref: "feat/remote".into(),
            head_oid: "0000000000000000000000000000000000000abc".into(),
            author: "alice".into(),
        }];
        app.dispatch_prs(crate::keymap::Action::Open);
        // the head isn't local: the open fetches the pull ref, not a branch
        let git = app.pending_git.take().expect("fetch queued");
        assert!(
            git.argv.iter().any(|a| a == "refs/pull/9/head"),
            "{:?}",
            git.argv
        );
        assert_eq!(app.pending_pr_open.as_ref().map(|p| p.number), Some(9));
    }

    #[test]
    fn commenting_through_the_modal_queues_a_forge_post() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let fixture = standard_fixture();
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    43\n}\n");
        fixture.stage("src/lib.rs");
        fixture.commit_all("bump");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        let base = app.review.vcs.resolve("HEAD~1").expect("base oid");
        app.open_pr_diff(3, &base, &head);
        assert!(app.diff.is_some());
        // a forge sync landing before the comment must not disturb posting
        app.sync_pr_comments(
            3,
            &[PrComment {
                id: "55".into(),
                path: "src/lib.rs".into(),
                line: Some(1),
                new_side: true,
                body: "remote".into(),
                author: "alice".into(),
                reply_to: None,
                start_line: None,
                thread_id: None,
                resolved: false,
                at: 1,
            }],
        );
        let press = |app: &mut App, code: KeyCode| {
            app.handle(crate::event::AppEvent::Key(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )));
        };
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('c'));
        assert!(app.modal.is_some(), "comment modal open");
        for ch in "ship it".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        assert!(
            app.pending_pr_posts.is_empty(),
            "comments stack locally until the review is submitted"
        );
        press(&mut app, KeyCode::Char('S'));
        assert!(
            matches!(
                app.modal,
                Some(crate::app::Modal::ReviewVerdict { number: 3 })
            ),
            "S opens the verdict picker: {:?}",
            app.modal
        );
        // approve, add a summary body, submit
        press(&mut app, KeyCode::Char('a'));
        assert!(
            matches!(app.modal, Some(crate::app::Modal::Input { .. })),
            "verdict leads to the summary prompt"
        );
        for ch in "lgtm".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.pending_pr_posts.len(), 1);
        let PrPost::Review { review, .. } = &app.pending_pr_posts[0] else {
            panic!("expected a review post: {:?}", app.pending_pr_posts);
        };
        assert_eq!(review.verdict, ReviewVerdict::Approve);
        assert_eq!(review.body, "lgtm");
        assert_eq!(review.comments.len(), 1);
        assert_eq!(review.comments[0].body, "ship it");
    }

    #[test]
    fn an_approval_needs_no_pending_comments_and_an_empty_summary_submits() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        app.pr_ranges.insert(4, (head.clone(), head));
        let press = |app: &mut App, code: KeyCode| {
            app.handle(crate::event::AppEvent::Key(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )));
        };
        app.pr_review_verdict_chosen(4, ReviewVerdict::Approve);
        assert!(
            matches!(app.modal, Some(crate::app::Modal::Input { .. })),
            "summary prompt opens"
        );
        press(&mut app, KeyCode::Enter);
        let PrPost::Review { review, .. } = &app.pending_pr_posts[0] else {
            panic!("expected a review post: {:?}", app.pending_pr_posts);
        };
        assert_eq!(review.verdict, ReviewVerdict::Approve);
        assert!(review.body.is_empty());
        assert!(review.comments.is_empty());
    }

    #[test]
    fn request_changes_with_nothing_to_say_is_rejected() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        app.pr_ranges.insert(4, (head.clone(), head));
        app.queue_pr_review(4, ReviewVerdict::RequestChanges, "");
        assert!(app.pending_pr_posts.is_empty());
        let message = app.message.clone().expect("message");
        assert!(message.text.contains("needs a summary"), "{message:?}");
    }

    #[test]
    fn next_comment_then_reply_opens_the_modal_in_a_pr_view() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let fixture = crate::test_support::Fixture::new();
        fixture.write("app.py", "def greet(name):\n    return f\"hello {name}\"\n");
        fixture.stage("app.py");
        fixture.commit_all("initial");
        fixture.write("app.py", "def greet(name):\n    return f\"hi {name}!\"\n");
        fixture.stage("app.py");
        fixture.commit_all("tweak");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head");
        let base = app.review.vcs.resolve("HEAD~1").expect("base");
        app.open_pr_diff(9, &base, &head);
        app.sync_pr_comments(
            9,
            &[PrComment {
                id: "70".into(),
                path: "app.py".into(),
                line: Some(2),
                new_side: true,
                body: "hm?".into(),
                author: "alice".into(),
                reply_to: None,
                start_line: None,
                thread_id: None,
                resolved: false,
                at: 1,
            }],
        );
        if let Some(diff) = app.diff.as_mut() {
            diff.ensure_rows(&app.review);
        }
        let press = |app: &mut App, code: KeyCode| {
            app.handle(crate::event::AppEvent::Key(KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )));
        };
        press(&mut app, KeyCode::Char(']'));
        press(&mut app, KeyCode::Char('r'));
        let kinds: Vec<String> = app
            .diff
            .as_ref()
            .map(|d| {
                d.rows()
                    .iter()
                    .map(|r| format!("{r:?}").chars().take(30).collect())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            app.modal.is_some(),
            "reply modal after ]+r; cursor={:?} rows={kinds:?}",
            app.diff.as_ref().map(|d| d.cursor)
        );
    }

    #[test]
    fn a_moved_pr_head_queues_a_refetch_and_a_matching_head_does_not() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        app.pr_ranges.insert(5, ("base".into(), head.clone()));
        let pr = crate::ci::PullRequest {
            number: 5,
            title: "t".into(),
            url: None,
            base_ref: "main".into(),
            head_ref: "feat/x".into(),
            head_oid: head.clone(),
            author: "alice".into(),
        };
        app.on_pr_head_seen(&pr);
        assert!(app.pending_git.is_none(), "same head: nothing to do");

        let moved = crate::ci::PullRequest {
            head_oid: "1111111111111111111111111111111111111111".into(),
            ..pr
        };
        app.on_pr_head_seen(&moved);
        let git = app.pending_git.take().expect("refetch queued");
        assert!(
            git.argv.iter().any(|a| a == "refs/pull/5/head"),
            "{:?}",
            git.argv
        );
        assert_eq!(app.pending_pr_open.as_ref().map(|p| p.number), Some(5));
        // a second sighting while the fetch is pending must not re-queue
        app.on_pr_head_seen(&moved);
        assert!(app.pending_git.is_none(), "fetch already in flight");
    }

    /// The full submit→ack→resync event chain, as the live app sees it.
    #[test]
    fn submit_ack_and_resync_round_trip() {
        let fixture = standard_fixture();
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    43\n}\n");
        fixture.stage("src/lib.rs");
        fixture.commit_all("bump");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        let base = app.review.vcs.resolve("HEAD~1").expect("base oid");
        app.open_pr_diff(3, &base, &head);
        let source = ReviewSource::pr(3);
        let single = Anchor {
            file: "src/lib.rs".into(),
            line: Some(2),
            line_end: None,
            on_old_side: false,
            line_text: None,
        };
        let range = Anchor {
            file: "src/lib.rs".into(),
            line: Some(1),
            line_end: Some(3),
            on_old_side: false,
            line_text: None,
        };
        {
            let session = app.review.session_for_mut(&source);
            session.add_comment(single, "me", "lab single comment");
            session.add_comment(range, "me", "lab range comment");
        }
        app.queue_pr_review(3, ReviewVerdict::Comment, "lab review body");
        assert_eq!(app.pending_pr_posts.len(), 1);
        let post = app.pending_pr_posts.remove(0);
        let flow = app.handle(crate::event::AppEvent::PrPosted {
            post: Box::new(post),
            result: Ok(None),
        });
        assert!(matches!(flow, crate::app::Flow::Continue));
        assert!(
            app.review.session_for(&source).comments.is_empty(),
            "locals handed to the forge"
        );
        let listing = vec![
            PrComment {
                id: "900".into(),
                path: "src/lib.rs".into(),
                line: Some(2),
                start_line: None,
                new_side: true,
                body: "lab single comment".into(),
                author: "me".into(),
                reply_to: None,
                thread_id: Some("T_a".into()),
                resolved: false,
                at: 5,
            },
            PrComment {
                id: "901".into(),
                path: "src/lib.rs".into(),
                line: Some(3),
                start_line: Some(1),
                new_side: true,
                body: "lab range comment".into(),
                author: "me".into(),
                reply_to: None,
                thread_id: Some("T_b".into()),
                resolved: false,
                at: 6,
            },
        ];
        let pr = crate::ci::PullRequest {
            number: 3,
            title: "t".into(),
            url: None,
            base_ref: "main".into(),
            head_ref: "feat".into(),
            head_oid: head.clone(),
            author: "me".into(),
        };
        app.handle(crate::event::AppEvent::PrComments {
            number: 3,
            comments: listing,
            pr: Some(pr),
        });
        let session = app.review.session_for(&source);
        assert_eq!(session.comments.len(), 2, "forge copies imported");
        assert!(session.comments.iter().all(|c| c.remote_id.is_some()));
        let ranged = session
            .comments
            .iter()
            .find(|c| c.body == "lab range comment")
            .expect("range comment");
        assert_eq!(ranged.anchor.line, Some(1));
        assert_eq!(ranged.anchor.line_end, Some(3));
    }

    #[test]
    fn sync_keeps_local_replies_and_status_on_forge_roots() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let listing = [PrComment {
            id: "100".into(),
            path: "app.txt".into(),
            line: Some(2),
            new_side: true,
            body: "remote root".into(),
            author: "alice".into(),
            reply_to: None,
            start_line: None,
            thread_id: None,
            resolved: false,
            at: 10,
        }];
        app.sync_pr_comments(7, &listing);
        let source = ReviewSource::pr(7);
        {
            let session = app.review.session_for_mut(&source);
            let id = session.comments[0].id.clone();
            session.reply(&id, "me", "local reply not yet sent");
            session.comments[0].status = CommentStatus::Resolved;
        }
        // the next poll returns the same listing: the unsent reply must
        // survive, and the stale local Resolved yields to the forge's Open
        app.sync_pr_comments(7, &listing);
        let session = app.review.session_for(&source);
        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.comments[0].status, CommentStatus::Open);
        assert_eq!(session.comments[0].replies.len(), 1);
        assert_eq!(
            session.comments[0].replies[0].body,
            "local reply not yet sent"
        );

        // an optimistic flip whose post is still in flight is not stale:
        // the poll must not flicker it back — in either direction
        {
            let session = app.review.session_for_mut(&source);
            session.comments[0].status = CommentStatus::Resolved;
        }
        let id = app.review.session_for(&source).comments[0].id.clone();
        app.pr_posts_inflight.insert(format!("res-{id}-true"));
        app.sync_pr_comments(7, &listing);
        assert_eq!(
            app.review.session_for(&source).comments[0].status,
            CommentStatus::Resolved
        );
        app.pr_posts_inflight.clear();

        // the unresolve direction: forge still says resolved while the
        // unresolve post is in flight — the local Open must survive
        let resolved_listing = [PrComment {
            resolved: true,
            thread_id: Some("T_1".into()),
            ..listing[0].clone()
        }];
        {
            let session = app.review.session_for_mut(&source);
            session.comments[0].status = CommentStatus::Open;
        }
        app.pr_posts_inflight.insert(format!("res-{id}-false"));
        app.sync_pr_comments(7, &resolved_listing);
        assert_eq!(
            app.review.session_for(&source).comments[0].status,
            CommentStatus::Open,
            "in-flight unresolve survives a stale poll"
        );
    }

    #[test]
    fn forge_resolved_threads_import_resolved_and_r_toggles_back() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        app.pr_ranges.insert(7, (head.clone(), head));
        app.sync_pr_comments(
            7,
            &[PrComment {
                id: "100".into(),
                path: "app.txt".into(),
                line: Some(2),
                new_side: true,
                body: "remote root".into(),
                author: "alice".into(),
                reply_to: None,
                start_line: None,
                thread_id: Some("T_1".into()),
                resolved: true,
                at: 10,
            }],
        );
        let source = ReviewSource::pr(7);
        assert_eq!(
            app.review.session_for(&source).comments[0].status,
            CommentStatus::Resolved,
            "forge resolution imports"
        );
        // reopening queues an unresolve against the mapped thread
        app.queue_pr_resolve(7, "remote-100", false);
        let PrPost::Resolve {
            thread_id,
            resolved,
            ..
        } = &app.pending_pr_posts[0]
        else {
            panic!("expected a resolve post: {:?}", app.pending_pr_posts);
        };
        assert_eq!(thread_id, "T_1");
        assert!(!resolved);
    }

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
                    start_line: None,
                    thread_id: None,
                    resolved: false,
                    at: 10,
                },
                PrComment {
                    id: "101".into(),
                    path: "app.txt".into(),
                    line: Some(2),
                    new_side: true,
                    body: "remote reply".into(),
                    author: "bob".into(),
                    start_line: None,
                    thread_id: None,
                    resolved: false,
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
            .add_comment(anchor, "me", "needs work")
            .id
            .clone();
        app.open_pr_diff(7, &head, &head);
        assert!(app.diff.is_some(), "the PR diff view opened");
        app.queue_pr_review(7, ReviewVerdict::Comment, "");
        assert_eq!(app.pending_pr_posts.len(), 1);
        app.queue_pr_review(7, ReviewVerdict::Comment, "");
        assert_eq!(
            app.pending_pr_posts.len(),
            1,
            "inflight set blocks re-queue"
        );

        // a successful submit hands the comment to the forge: the local copy
        // goes away and the immediate resync is queued
        let post = app.pending_pr_posts.remove(0);
        app.on_pr_posted(&post, Ok(None));
        let session = app.review.session_for(&source);
        assert!(session.comments.iter().all(|c| c.id != local_id));
        assert!(matches!(
            app.pending_ci,
            Some(crate::app::CiRequest::PrComments(7))
        ));
    }
}
