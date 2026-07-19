//! Push/pull orchestration: resolve the remote, ask before anything that could
//! fail or destroy work, and turn a rejected push/pull into an actionable
//! dialog instead of a dead error.

use super::{App, GitOp, Modal, PendingOp, RemotePurpose, fuzzy};

impl App {
    pub(crate) fn push(&mut self) {
        if self.head.upstream.is_some() {
            self.queue_network("push", vec!["git".into(), "push".into()]);
        } else {
            self.push_set_upstream();
        }
    }

    pub(crate) fn push_set_upstream(&mut self) {
        let Some(branch) = self.head.branch.clone() else {
            self.error("HEAD is detached; nothing to push");
            return;
        };
        let remotes = self.review.vcs.remotes().unwrap_or_default();
        match remotes.as_slice() {
            [] => self.info("no remote configured"),
            [remote] => {
                let argv = push_upstream_argv(remote);
                self.modal = Some(Modal::Confirm {
                    message: format!("Push {branch} to {remote} and set it as upstream?"),
                    on_confirm: PendingOp::RunGit {
                        label: "push -u".into(),
                        argv,
                    },
                });
            }
            _ => self.open_remote_list(remotes, RemotePurpose::SetUpstreamPush),
        }
    }

    pub(crate) fn pull(&mut self) {
        if self.head.upstream.is_some() {
            self.queue_network("pull", vec!["git".into(), "pull".into()]);
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
        self.queue_network(
            "pull",
            vec!["git".into(), "pull".into(), remote.into(), branch],
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
                self.queue_network("push -u", push_upstream_argv(remote));
            }
            RemotePurpose::Pull => self.pull_from(remote),
        }
    }

    pub(super) fn pull_rebase(&mut self) {
        self.queue_network(
            "pull --rebase",
            vec!["git".into(), "pull".into(), "--rebase".into()],
        );
    }

    pub(super) fn pull_merge(&mut self) {
        self.queue_network(
            "pull",
            vec!["git".into(), "pull".into(), "--no-rebase".into()],
        );
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

fn push_upstream_argv(remote: &str) -> Vec<String> {
    vec![
        "git".into(),
        "push".into(),
        "-u".into(),
        remote.into(),
        "HEAD".into(),
    ]
}

fn with_force_lease(mut argv: Vec<String>) -> Vec<String> {
    let at = argv
        .iter()
        .position(|a| a == "push")
        .map_or(argv.len(), |i| i + 1);
    argv.insert(at, "--force-with-lease".into());
    argv
}
