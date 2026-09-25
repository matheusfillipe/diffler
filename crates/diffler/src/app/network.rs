//! Push/pull orchestration: resolve the remote, ask before anything that could
//! fail or destroy work, and turn a rejected push/pull into an actionable
//! dialog instead of a dead error.

use diffler_core::vcs::NetworkOp;

use super::{App, GitOp, Modal, PendingOp, RemotePurpose, fuzzy};

impl App {
    pub(crate) fn push(&mut self) {
        if self.head.upstream.is_some() {
            self.queue_network_op("push", NetworkOp::Push);
        } else {
            self.push_set_upstream();
        }
    }

    pub(crate) fn push_set_upstream(&mut self) {
        if self.declines_jj_network() {
            return;
        }
        let Some(branch) = self.head.branch.clone() else {
            self.error("HEAD is detached; nothing to push");
            return;
        };
        let remotes = self.review.vcs.remotes().unwrap_or_default();
        match remotes.as_slice() {
            [] => self.info("no remote configured"),
            [remote] => self.confirm_push_upstream(&branch, remote),
            _ => self.open_remote_list(remotes, RemotePurpose::SetUpstreamPush),
        }
    }

    fn confirm_push_upstream(&mut self, branch: &str, remote: &str) {
        let op = NetworkOp::PushSetUpstream {
            remote: remote.to_owned(),
        };
        match self.review.vcs.network_argv(op) {
            Ok(argv) => {
                self.modal = Some(Modal::Confirm {
                    message: format!("Push {branch} to {remote} and set it as upstream?"),
                    on_confirm: PendingOp::RunGit {
                        label: "push -u".into(),
                        argv,
                    },
                });
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    pub(crate) fn pull(&mut self) {
        if self.declines_jj_network() {
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

    /// Resolve `op`'s argv through the backend and queue it, showing its
    /// decline (jj refuses every push/pull) as an error instead.
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
        self.info(format!("running git {label}…"));
        self.pending_git = Some(GitOp { label, argv });
    }

    /// Push and pull run the git CLI, which in a jj repo would move git's
    /// HEAD and branches behind jj's back. Every push/pull variant declines
    /// identically, so probing with a bare `Push` answers for all of them,
    /// before a caller that must not show a confirm dialog or remote picker
    /// for a repo that will only reject it ever resolves one.
    pub(super) fn declines_jj_network(&mut self) -> bool {
        match self.review.vcs.network_argv(NetworkOp::Push) {
            Ok(_) => false,
            Err(err) => {
                self.error(err.to_string());
                true
            }
        }
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
