//! Push/pull orchestration: resolve the remote, ask before anything that could
//! fail or destroy work, and turn a rejected push/pull into a recovery dialog.

use std::fmt::Write as _;

use diffler_core::vcs::{NetworkOp, PullOutcome, PullTarget};

use super::{App, GitOp, Modal, PendingOp, RemotePurpose, Severity, StatusMessage, fuzzy};

/// The labels a network op carries; in a jj repo each of them runs `jj`.
pub(super) fn is_jj_network_label(label: &str) -> bool {
    matches!(label, "push" | "push -u" | "pull" | "fetch" | "fetch --all")
}

/// jj reports on stderr in blocks: the `Error:` line on failure, else the
/// first line, joined with the next when it only introduces a list.
pub(super) fn jj_summary(output: &str) -> String {
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    if let Some(error) = lines.clone().find(|line| line.starts_with("Error:")) {
        return error.to_owned();
    }
    match (lines.next(), lines.next()) {
        (Some(head), Some(next)) if head.ends_with(':') => format!("{head} {next}"),
        (Some(head), _) => head.to_owned(),
        (None, _) => String::new(),
    }
}

impl App {
    /// jj picks the remote itself, from `git.push` or else `origin`.
    pub(crate) fn push(&mut self) {
        if self.head.upstream.is_some() || !self.review.vcs.has_index() {
            self.queue_network_op("push", NetworkOp::Push);
        } else {
            self.push_set_upstream();
        }
    }

    /// jj picks what to push itself, once confirmed, so a git branch is the
    /// only name the dialog can give.
    pub(crate) fn push_set_upstream(&mut self) {
        let branch = if self.review.vcs.has_index() {
            let Some(branch) = self.head.branch.clone() else {
                self.error("HEAD is detached; nothing to push");
                return;
            };
            Some(branch)
        } else {
            None
        };
        let remotes = self.review.vcs.remotes().unwrap_or_default();
        match remotes.as_slice() {
            [] => self.info("no remote configured"),
            [remote] => self.confirm_push_upstream(branch.as_deref(), remote),
            _ => self.open_remote_list(remotes, RemotePurpose::SetUpstreamPush),
        }
    }

    fn confirm_push_upstream(&mut self, branch: Option<&str>, remote: &str) {
        let message = match branch {
            Some(branch) => format!("Push {branch} to {remote} and set it as upstream?"),
            None => format!("Push to {remote} and track the bookmark there?"),
        };
        self.modal = Some(Modal::Confirm {
            message,
            on_confirm: PendingOp::Network {
                label: "push -u".into(),
                op: NetworkOp::PushSetUpstream {
                    remote: remote.to_owned(),
                },
            },
        });
    }

    /// jj fetches the remote the nearest bookmark tracks, then
    /// [`Self::finish_jj_pull`] rebases `@` onto that bookmark.
    pub(crate) fn pull(&mut self) {
        if !self.review.vcs.has_index() {
            let target = match self.review.vcs.pull_target() {
                Ok(target) => target,
                Err(err) => {
                    self.error(err.to_string());
                    return;
                }
            };
            let op = match &target {
                Some(PullTarget {
                    bookmark,
                    remote: Some(remote),
                }) => NetworkOp::PullFrom {
                    remote: remote.clone(),
                    branch: bookmark.clone(),
                },
                _ => NetworkOp::Pull,
            };
            self.pending_jj_pull = target;
            self.queue_network_op("pull", op);
            return;
        }
        if self.head.upstream.is_some() {
            self.queue_network_op("pull", NetworkOp::Pull);
            return;
        }
        let remotes = self.review.vcs.remotes().unwrap_or_default();
        match remotes.as_slice() {
            [] => self.info("no remote configured"),
            [remote] => self.pull_from(&remote.clone()),
            _ => self.open_remote_list(remotes, RemotePurpose::Pull),
        }
    }

    fn pull_from(&mut self, remote: &str) {
        let Some(branch) = self.head.branch.clone() else {
            self.error("HEAD is detached; nothing to pull");
            return;
        };
        self.queue_network_op(
            "pull",
            NetworkOp::PullFrom {
                remote: remote.to_owned(),
                branch,
            },
        );
    }

    fn open_remote_list(&mut self, remotes: Vec<String>, purpose: RemotePurpose) {
        let mut list = fuzzy::FuzzyList::default();
        list.rerank(&remotes);
        self.modal = Some(Modal::RemoteList {
            remotes,
            list,
            purpose,
        });
    }

    pub(super) fn remote_chosen(&mut self, remote: &str, purpose: RemotePurpose) {
        match purpose {
            RemotePurpose::SetUpstreamPush => {
                self.queue_network_op(
                    "push -u",
                    NetworkOp::PushSetUpstream {
                        remote: remote.to_owned(),
                    },
                );
            }
            RemotePurpose::Pull => self.pull_from(remote),
        }
    }

    pub(super) fn pull_rebase(&mut self) {
        self.queue_network_op("pull --rebase", NetworkOp::PullRebase);
    }

    pub(super) fn pull_merge(&mut self) {
        self.queue_network_op("pull", NetworkOp::PullMerge);
    }

    /// Resolve `op`'s argv through the backend and queue it. A backend that
    /// declines (jj on push/pull) shows the error.
    pub(crate) fn queue_network_op(&mut self, label: impl Into<String>, op: NetworkOp) {
        match self.review.vcs.network_argv(op) {
            Ok(argv) => self.queue_network(label, argv),
            Err(err) => self.error(err.to_string()),
        }
    }

    /// Queue a git op and, when it is a push, remember its argv so a rejection
    /// can retry with `--force-with-lease` against the same target.
    pub(crate) fn queue_network(&mut self, label: impl Into<String>, argv: Vec<String>) {
        let label = label.into();
        if argv.get(1).map(String::as_str) == Some("push") {
            self.last_push_argv = Some(argv.clone());
        }
        let program = argv.first().map_or("git", String::as_str);
        self.info(format!("running {program} {label}…"));
        self.pending_git = Some(GitOp { label, argv });
    }

    /// The fetch of a jj pull finished; `target` is what it was queued for.
    pub(super) fn finish_jj_pull(&mut self, target: Option<PullTarget>) {
        self.queue_refresh();
        let Some(PullTarget { bookmark, remote }) = target else {
            self.info("fetched; bookmark a change under @ to rebase it on pull");
            return;
        };
        let Some(remote) = remote else {
            self.info(format!(
                "fetched; track a remote bookmark for {bookmark} to rebase on pull"
            ));
            return;
        };
        match self.review.vcs.rebase_onto_remote(&bookmark, &remote) {
            Ok(PullOutcome::Gone { onto }) => self.warn(format!(
                "fetched; the remote deleted {onto}, so @ stays where it is"
            )),
            Ok(PullOutcome::UpToDate { onto }) => {
                self.info(format!("fetched; @ already sits on {onto}"));
            }
            Ok(PullOutcome::Rebased {
                onto,
                conflicts,
                bookmark_conflicted,
            }) => {
                let mut text = format!("rebased @ onto {onto}");
                if conflicts > 0 {
                    let noun = if conflicts == 1 { "change" } else { "changes" };
                    // we drop the result, since writing to a String cannot fail
                    let _ = write!(text, "; resolve the conflicts in {conflicts} {noun}");
                }
                if bookmark_conflicted {
                    let _ = write!(
                        text,
                        "; pick where {bookmark} goes with jj bookmark set, since it moved on both sides"
                    );
                }
                if conflicts > 0 || bookmark_conflicted {
                    self.warn(text);
                } else {
                    self.info(text);
                }
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    fn warn(&mut self, text: String) {
        self.message = Some(StatusMessage {
            text,
            severity: Severity::Warning,
        });
    }

    /// A failed push/pull: open the recovery dialog its error calls for.
    /// Returns true when a dialog was opened, suppressing the raw error.
    pub(super) fn network_recovery(&mut self, label: &str, output: &str) -> bool {
        let out = output.to_ascii_lowercase();
        let no_upstream = out.contains("no upstream")
            || out.contains("no tracking information")
            || out.contains("set the upstream");
        if label.starts_with("push") {
            if label.contains("force") {
                return false; // a rejected force-with-lease is a real conflict
            }
            // a pull-request push only ever publishes the branch as it stands
            if label == Self::PR_CREATE_PUSH {
                return false;
            }
            if no_upstream {
                self.push_set_upstream();
                return true;
            }
            if out.contains("non-fast-forward")
                || out.contains("updates were rejected")
                || out.contains("fetch first")
                || out.contains("[rejected]")
            {
                let Some(argv) = self.last_push_argv.clone() else {
                    return false;
                };
                self.modal = Some(Modal::Confirm {
                    message: "The remote has commits you don't have. \
                              Force-push with --force-with-lease?"
                        .into(),
                    on_confirm: PendingOp::RunGit {
                        label: "push --force-with-lease".into(),
                        argv: with_force_lease(argv),
                    },
                });
                return true;
            }
        } else if label.starts_with("pull") {
            if no_upstream {
                self.pull();
                return true;
            }
            if out.contains("diverg")
                || out.contains("reconcile")
                || out.contains("not possible to fast-forward")
            {
                let upstream = self
                    .head
                    .upstream
                    .clone()
                    .unwrap_or_else(|| "the remote".into());
                self.modal = Some(Modal::PullDiverged { upstream });
                return true;
            }
        }
        false
    }
}

fn with_force_lease(mut argv: Vec<String>) -> Vec<String> {
    let at = argv
        .iter()
        .position(|a| a == "push")
        .map_or(argv.len(), |i| i + 1);
    argv.insert(at, "--force-with-lease".into());
    argv
}
