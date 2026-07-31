//! Opening a pull request from the TUI: check the branch is in a state a
//! forge will accept, fill the fields from the commits the branch carries,
//! and let the human correct any of it before it goes out.

use diffler_core::vcs::LogEntry;

use super::App;
use crate::ci::NewPullRequest;

/// The field the cursor sits on in the create form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrField {
    Base,
    Title,
    Body,
    Draft,
}

impl PrField {
    pub(crate) const ORDER: [Self; 4] = [Self::Base, Self::Title, Self::Body, Self::Draft];

    pub(crate) fn step(self, down: bool) -> Self {
        let at = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        let next = if down {
            (at + 1).min(Self::ORDER.len() - 1)
        } else {
            at.saturating_sub(1)
        };
        Self::ORDER.get(next).copied().unwrap_or(Self::Title)
    }
}

/// The pull request being composed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDraft {
    pub base: String,
    pub head: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    /// Commits `head` carries over `base`, for the form's summary line.
    pub commits: usize,
    /// The remote lacks this branch at the local commit, so creating pushes first.
    pub needs_push: bool,
    pub field: PrField,
}

impl PrDraft {
    pub(crate) fn request(&self) -> NewPullRequest {
        NewPullRequest {
            base: self.base.clone(),
            head: self.head.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            draft: self.draft,
        }
    }
}

/// Title and body the way `gh` and `glab` build them.
fn defaults_from(commits: &[LogEntry], branch: &str) -> (String, String) {
    if let [only] = commits {
        (only.subject.clone(), String::new())
    } else {
        let title = humanize(branch);
        let body = commits.iter().rev().fold(String::new(), |mut acc, c| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "- {}", c.subject);
            acc
        });
        (title, body)
    }
}

/// A branch name as prose: `feat/pr-create` reads `pr create`.
fn humanize(branch: &str) -> String {
    let tail = branch.rsplit('/').next().unwrap_or(branch);
    tail.replace(['-', '_'], " ")
}

impl App {
    /// Label of the push a pending create waits on. Dedicated, because
    /// `git_finished` must not let an unrelated push consume the slot.
    pub(crate) const PR_CREATE_PUSH: &'static str = "push for the pull request";

    pub(crate) fn create_pr_start(&mut self) {
        let Some(remote) = self.ci_remotes().first().cloned() else {
            self.info("no forge detected for this repo");
            return;
        };
        let Some(head) = self.head.branch.clone() else {
            self.error("HEAD is detached; check out a branch to open a pull request");
            return;
        };
        let base = match self.review.vcs.default_branch(&remote.name) {
            Ok(Some(base)) => base,
            Ok(None) => {
                self.error(format!("no default branch on {}", remote.name));
                return;
            }
            Err(err) => {
                self.error(err.to_string());
                return;
            }
        };
        if base == head {
            self.info(format!("already on {base}; branch first"));
            return;
        }
        // the pull request merges into the remote's branch, so the commits it
        // carries count from there; a local branch of the same name can be
        // stale, or missing entirely once it has been pruned
        let base_rev = self.remote_rev(&remote.name, &base);
        let commits = match self.review.vcs.commits_between(&base_rev, &head) {
            Ok(commits) => commits,
            Err(err) => {
                self.error(err.to_string());
                return;
            }
        };
        if commits.is_empty() {
            self.info(format!("{head} has nothing {base} doesn't"));
            return;
        }
        let (title, body) = defaults_from(&commits, &head);
        let needs_push = !self.branch_is_on_remote(&remote.name, &head);
        self.modal = Some(super::Modal::CreatePr {
            draft: Box::new(PrDraft {
                base,
                head,
                title,
                body,
                draft: false,
                commits: commits.len(),
                needs_push,
                field: PrField::Title,
            }),
        });
    }

    /// `branch` as the remote holds it, falling back to the bare name when
    /// there is no tracking ref to read.
    fn remote_rev(&self, remote: &str, branch: &str) -> String {
        let tracking = format!("refs/remotes/{remote}/{branch}");
        if self.review.vcs.resolve(&tracking).is_ok() {
            tracking
        } else {
            branch.to_owned()
        }
    }

    /// `git checkout -b` inherits the parent's upstream, so the tracking ref
    /// itself has to exist and match the local commit.
    fn branch_is_on_remote(&self, remote: &str, branch: &str) -> bool {
        let there = self
            .review
            .vcs
            .resolve(&format!("refs/remotes/{remote}/{branch}"));
        match (there, self.review.vcs.resolve(branch)) {
            (Ok(there), Ok(here)) => there == here,
            _ => false,
        }
    }

    /// Send the composed pull request, pushing the branch first when the forge
    /// has never seen it.
    pub(crate) fn create_pr_submit(&mut self) {
        let Some(super::Modal::CreatePr { draft }) = self.modal.take() else {
            return;
        };
        if draft.title.trim().is_empty() {
            self.info("a pull request needs a title");
            self.modal = Some(super::Modal::CreatePr { draft });
            return;
        }
        let request = draft.request();
        if !draft.needs_push {
            self.queue_pr_create(request);
            return;
        }
        let Some(remote) = self.ci_remotes().first().map(|r| r.name.clone()) else {
            self.error("no remote to push the branch to");
            return;
        };
        // arm the slot and queue the push it waits on together: a slot armed
        // ahead of a dialog stays armed when the dialog is declined, and the
        // next push of any kind would then open a pull request nobody asked for
        self.pending_pr_create = Some(Box::new(request));
        self.queue_network(
            Self::PR_CREATE_PUSH,
            super::network::push_upstream_argv(&remote),
        );
    }

    pub(crate) fn queue_pr_create(&mut self, request: NewPullRequest) {
        self.info(format!("opening {} → {}…", request.head, request.base));
        self.pending_ci = Some(super::CiRequest::CreatePr(Box::new(request)));
    }

    /// The push a create was waiting on landed; send the pull request now.
    pub(crate) fn pr_create_after_push(&mut self) {
        if let Some(request) = self.pending_pr_create.take() {
            self.queue_pr_create(*request);
        }
    }

    pub(crate) fn on_pr_created(&mut self, result: Result<crate::ci::PullRequest, String>) {
        match result {
            Ok(pr) => {
                self.info(format!("opened #{}: {}", pr.number, pr.title));
                // reviewing needs the head commit; a provider that answers
                // without one leaves the review to be opened from the PR list
                if pr.head_oid.is_empty() {
                    return;
                }
                self.open_pr_review_for(pr);
            }
            Err(err) => self.error(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(subject: &str) -> LogEntry {
        LogEntry {
            oid: "0".repeat(40),
            oid7: "0000000".to_owned(),
            refs: Vec::new(),
            subject: subject.to_owned(),
            author: "reviewer".to_owned(),
            time_unix: 0,
        }
    }

    #[test]
    fn a_lone_commit_titles_the_pull_request() {
        let (title, body) = defaults_from(&[entry("fix: stop dropping the reply")], "fix/reply");
        assert_eq!(title, "fix: stop dropping the reply");
        assert!(body.is_empty(), "one commit needs no summary of itself");
    }

    #[test]
    fn a_series_is_titled_by_its_branch_and_listed_oldest_first() {
        let commits = [entry("second"), entry("first")];
        let (title, body) = defaults_from(&commits, "feat/pr-create");
        assert_eq!(title, "pr create");
        assert_eq!(body, "- first\n- second\n");
    }

    #[test]
    fn fields_stop_at_both_ends() {
        assert_eq!(PrField::Base.step(false), PrField::Base);
        assert_eq!(PrField::Base.step(true), PrField::Title);
        assert_eq!(PrField::Draft.step(true), PrField::Draft);
    }

    #[test]
    fn a_branch_with_no_tracking_ref_still_needs_a_push() {
        let fixture = crate::test_support::standard_fixture();
        fixture.commit_all("base");
        let app = App::new(fixture.review(), crate::config::LoadedConfig::default());
        assert!(
            !app.branch_is_on_remote("origin", "main"),
            "no remote-tracking ref means the branch still has to be pushed"
        );
    }

    #[test]
    fn composing_without_a_forge_is_refused() {
        let fixture = crate::test_support::standard_fixture();
        fixture.commit_all("base");
        let mut app = App::new(fixture.review(), crate::config::LoadedConfig::default());
        app.create_pr_start();
        assert!(app.modal.is_none(), "no forge, no form");
    }

    /// The forge only sees a branch that is pushed, so the create waits for
    /// the push it queues.
    #[test]
    fn an_unpushed_branch_pushes_before_the_forge_call() {
        let fixture = crate::test_support::standard_fixture();
        fixture.remote("origin", "https://github.com/acme/widgets.git");
        let mut app = App::new(fixture.review(), crate::config::LoadedConfig::default());
        app.modal = Some(super::super::Modal::CreatePr {
            draft: Box::new(PrDraft {
                base: "main".to_owned(),
                head: "feat/x".to_owned(),
                title: "a title".to_owned(),
                body: String::new(),
                draft: false,
                commits: 1,
                needs_push: true,
                field: PrField::Title,
            }),
        });
        app.create_pr_submit();
        assert!(app.pending_pr_create.is_some(), "the request waits");
        assert!(
            !matches!(app.pending_ci, Some(super::super::CiRequest::CreatePr(_))),
            "nothing reaches the forge before the branch does"
        );
        // the push carries its own label so no other push can consume the slot
        assert_eq!(
            app.pending_git.as_ref().map(|op| op.label.as_str()),
            Some(App::PR_CREATE_PUSH),
        );
    }

    /// The forge call is addressed against the pushed head, which only the
    /// landed refresh carries.
    #[test]
    fn the_forge_call_waits_for_the_refresh_the_finished_push_queues() {
        let fixture = crate::test_support::standard_fixture();
        let mut app = App::new(fixture.review(), crate::config::LoadedConfig::default());
        app.pending_pr_create = Some(Box::new(NewPullRequest {
            base: "main".to_owned(),
            head: "feat/x".to_owned(),
            title: "a title".to_owned(),
            body: String::new(),
            draft: false,
        }));
        app.handle(crate::event::AppEvent::GitDone {
            label: App::PR_CREATE_PUSH.to_owned(),
            ok: true,
            output: String::new(),
        });
        assert!(app.pending_ci.is_none(), "the create holds for the refresh");
        app.settle_refresh();
        assert!(matches!(
            app.pending_ci,
            Some(super::super::CiRequest::CreatePr(_))
        ));
    }

    #[test]
    fn a_title_left_empty_keeps_the_form_open() {
        let fixture = crate::test_support::standard_fixture();
        let mut app = App::new(fixture.review(), crate::config::LoadedConfig::default());
        app.modal = Some(super::super::Modal::CreatePr {
            draft: Box::new(PrDraft {
                base: "main".to_owned(),
                head: "feat/x".to_owned(),
                title: "   ".to_owned(),
                body: String::new(),
                draft: false,
                commits: 1,
                needs_push: false,
                field: PrField::Title,
            }),
        });
        app.create_pr_submit();
        assert!(
            matches!(app.modal, Some(super::super::Modal::CreatePr { .. })),
            "the form stays up so the title can be filled in"
        );
        assert!(app.pending_ci.is_none());
    }
}
