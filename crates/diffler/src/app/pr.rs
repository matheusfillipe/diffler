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
            label: format!("fetch PR #{}", pr.number),
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
    ) -> super::Flow {
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
            root.status = prior.status;
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

    /// Submit everything pending in the active PR review as one forge review
    /// (plus individual replies to existing threads). Nothing posts until
    /// this runs — comments stack up locally so the forge sends one
    /// notification, not one per comment.
    pub(crate) fn submit_pr_review(&mut self) {
        let ReviewSource::Pr { number } = self.active_review_source() else {
            self.info("not reviewing a PR — nothing to submit");
            return;
        };
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
                    let line = comment.anchor.line_end.unwrap_or(line);
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
        if review_comments.is_empty() && replies.is_empty() {
            self.info("nothing pending to submit");
            return;
        }
        let total = review_comments.len() + replies.len();
        let mut posts = replies;
        if !review_comments.is_empty() {
            posts.push(PrPost::Review {
                review: crate::ci::NewPrReview {
                    number,
                    head_oid: head,
                    comments: review_comments,
                },
                comment_ids,
            });
        }
        for post in posts {
            let key = post_key(&post);
            if self.pr_posts_inflight.insert(key) {
                self.pending_pr_posts.push(post);
            }
        }
        self.info(format!("submitting review ({total} comments)…"));
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
            PrPost::Reply { number, .. } => *number,
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
                            r.remote_id = remote.map(|c| c.id);
                        }
                    }
                }
                if let Err(err) = self.review.save_for(&source) {
                    self.error(err.to_string());
                }
                if let Some(diff) = self.diff.as_mut() {
                    diff.invalidate();
                }
            }
            Err(err) => self.error(format!("posting to the PR failed: {err}")),
        }
    }
}

fn post_key(post: &PrPost) -> String {
    match post {
        PrPost::Review { review, .. } => format!("review-{}", review.number),
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
        assert_eq!(app.pending_pr_posts.len(), 1);
        let PrPost::Review { review, .. } = &app.pending_pr_posts[0] else {
            panic!("expected a review post: {:?}", app.pending_pr_posts);
        };
        assert_eq!(review.comments.len(), 1);
        assert_eq!(review.comments[0].body, "ship it");
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
            at: 10,
        }];
        app.sync_pr_comments(7, &listing);
        let source = ReviewSource::pr(7);
        {
            let session = app.review.session_for_mut(&source);
            let id = session.comments[0].id.clone();
            session.reply(&id, "me", "local reply not yet sent");
            session.comments[0].status = diffler_core::session::CommentStatus::Resolved;
        }
        // the next poll returns the same listing: local state must survive
        app.sync_pr_comments(7, &listing);
        let session = app.review.session_for(&source);
        assert_eq!(session.comments.len(), 1);
        assert_eq!(
            session.comments[0].status,
            diffler_core::session::CommentStatus::Resolved
        );
        assert_eq!(session.comments[0].replies.len(), 1);
        assert_eq!(
            session.comments[0].replies[0].body,
            "local reply not yet sent"
        );
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
        app.submit_pr_review();
        assert_eq!(app.pending_pr_posts.len(), 1);
        app.submit_pr_review();
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
