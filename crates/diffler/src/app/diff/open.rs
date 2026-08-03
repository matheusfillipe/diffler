//! Opening the diff screen on one review source: the working tree, a commit,
//! a range, a three-dot base, or a pull request.

use diffler_core::model::DiffModel;
use diffler_core::source::ReviewSource;

use super::{DiffView, Pane};
use crate::app::{App, Screen};

impl App {
    /// Open the full working-tree diff with the sidebar focused at the first
    /// file (`D` / section headers / commit-from-log model).
    pub(crate) fn open_working_tree_diff(&mut self, scope: Option<&str>) {
        self.open_working_tree_diff_focused(scope, Pane::List);
    }

    /// Open a single file's diff with the diff pane focused (`<cr>` on a
    /// status file row).
    pub(crate) fn open_working_tree_file(&mut self, path: &str) {
        self.open_working_tree_diff_focused(Some(path), Pane::Diff);
    }

    fn open_working_tree_diff_focused(&mut self, scope: Option<&str>, focus: Pane) {
        self.install_diff_view(ReviewSource::WorkingTree, None);
        let Some(view) = self.diff.as_mut() else {
            return;
        };
        if let Some(path) = scope
            && let Some(index) = self
                .review
                .model()
                .files
                .iter()
                .position(|f| f.path == path)
        {
            view.selected = index;
            view.invalidate();
            view.ensure_rows(&self.review);
        }
        view.focus = focus;
    }

    /// Shared ceremony for every diff opener: load the source's review state,
    /// build a fresh `DiffView` from the current file-layout config, install
    /// it as `self.diff`, and push the diff screen. On a source load failure
    /// the error is reported and nothing changes (`self.diff` stays `None` or
    /// keeps the previous view).
    fn install_diff_view(&mut self, source: ReviewSource, model: Option<DiffModel>) {
        if let Err(err) = self.review.ensure_source(&source) {
            self.error(err.to_string());
            return;
        }
        // a queued open can land while a comment is being written: the draft
        // rides along when it still belongs here, and is never dropped silently
        let open = self.diff.take();
        let same_source = open.as_ref().is_some_and(|open| open.source == source);
        let drafted_path = open
            .as_ref()
            .filter(|_| same_source)
            .and_then(|open| open.composer.as_ref().and(open.selected_path(&self.review)));
        let draft = open.and_then(|open| open.composer);
        let mut view = DiffView::new(
            source,
            model,
            &self.review,
            self.config.ui.diff_file_layout,
            self.config.ui.side_by_side,
        );
        match draft {
            Some(draft) if same_source => {
                // the composer only draws on the file it is anchored to, so
                // the rebuilt view has to land back on it
                if let Some(index) = drafted_path.and_then(|path| {
                    view.model(&self.review)
                        .files
                        .iter()
                        .position(|file| file.path == path)
                }) {
                    view.selected = index;
                }
                view.composer = Some(draft);
                view.invalidate();
                view.ensure_rows(&self.review);
            }
            Some(draft) if !draft.buffer.trim().is_empty() => {
                self.error("the diff moved; your unsent draft was dropped");
            }
            _ => {}
        }
        self.diff = Some(view);
        self.push_screen(Screen::Diff);
    }

    pub(crate) fn open_commit_diff(&mut self, oid: &str) {
        match self.review.vcs.commit_diff(oid) {
            Ok(model) => self.install_diff_view(ReviewSource::commit(oid), Some(model)),
            Err(err) => self.error(err.to_string()),
        }
    }

    /// Review everything the working tree carries over `rev`: the branch's
    /// commits plus whatever is still uncommitted. The model tracks edits, so
    /// the off-thread refresh recomputes it (see `App::against_rev`).
    pub(crate) fn open_against_diff(&mut self, rev: &str) {
        match diffler_core::vcs::against_diff(self.review.vcs.as_ref(), rev) {
            Ok(model) => {
                let source = ReviewSource::against(rev);
                if let Some(diff) = self.diff.as_mut().filter(|d| d.source == source) {
                    diff.commit_model = Some(model);
                    diff.invalidate();
                    diff.ensure_rows(&self.review);
                } else {
                    self.install_diff_view(source, Some(model));
                }
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    /// The `Against` diff for `rev` outside the render path (agent tool calls):
    /// the open view's model when it is showing that rev, else a fresh compute.
    /// A backend error degrades to an empty diff, like the cached sources.
    pub(crate) fn against_model_for(&self, rev: &str) -> DiffModel {
        self.diff
            .as_ref()
            .filter(|d| d.source == ReviewSource::against(rev))
            .and_then(|d| d.commit_model.clone())
            .unwrap_or_else(|| {
                diffler_core::vcs::against_diff(self.review.vcs.as_ref(), rev).unwrap_or_default()
            })
    }

    /// The rev of the open `Against` review, so the refresh worker recomputes
    /// its diff alongside the working tree.
    pub fn against_rev(&self) -> Option<&str> {
        match self.diff.as_ref().map(|d| &d.source) {
            Some(ReviewSource::Against { rev }) => Some(rev),
            _ => None,
        }
    }

    /// Open the combined diff of a contiguous commit range (oldest to newest,
    /// full oids), pinned like a single commit's diff.
    pub(crate) fn open_range_diff(&mut self, oldest: &str, newest: &str) {
        match self.review.vcs.range_diff(oldest, newest) {
            Ok(model) => self.install_diff_view(ReviewSource::range(oldest, newest), Some(model)),
            Err(err) => self.error(err.to_string()),
        }
    }

    /// Review the branch's open PR: diff `merge-base..head` under the stable
    /// `pr-<n>` source. A head we don't have yet is fetched first (forges
    /// serve `refs/pull/<n>/head`) and the open retries when the fetch lands.
    pub(crate) fn open_pr_review(&mut self) {
        let Some(pr) = self.pr.clone() else {
            self.info("no open PR detected for this branch");
            return;
        };
        self.open_pr_review_for(pr);
    }

    /// Review any PR, including one whose branch was never checked out; the
    /// diff needs only the fetched objects.
    pub(crate) fn open_pr_review_for(&mut self, pr: crate::ci::PullRequest) {
        if let Some((base, head)) = self.resolve_pr_range(&pr) {
            self.open_pr_diff(pr.number, &base, &head);
        } else {
            let remote = self
                .ci_remotes
                .first()
                .map_or_else(|| "origin".to_owned(), |r| r.name.clone());
            let refspec = format!("refs/pull/{}/head", pr.number);
            let base_ref = pr.base_ref.clone();
            let label = Self::pr_fetch_label(pr.number);
            self.pending_pr_open = Some(pr);
            // the base ref comes along so merge-base reflects the forge's
            // view, not however stale the last fetch left it
            self.pending_git = Some(crate::app::GitOp {
                label,
                argv: vec![
                    "git".to_owned(),
                    "fetch".to_owned(),
                    remote,
                    refspec,
                    base_ref,
                ],
            });
        }
    }

    /// `(merge_base, head)` for the PR against the local objects; `None` when
    /// the head hasn't been fetched yet.
    pub(crate) fn resolve_pr_range(&self, pr: &crate::ci::PullRequest) -> Option<(String, String)> {
        let head = self.review.vcs.resolve(&pr.head_oid).ok()?;
        let base_tip = self
            .ci_remotes
            .first()
            .and_then(|r| {
                self.review
                    .vcs
                    .resolve(&format!("refs/remotes/{}/{}", r.name, pr.base_ref))
                    .ok()
            })
            .or_else(|| self.review.vcs.resolve(&pr.base_ref).ok())?;
        let base = self.review.vcs.merge_base(&base_tip, &head).ok()?;
        Some((base, head))
    }

    pub(crate) fn open_pr_diff(&mut self, number: u64, base: &str, head: &str) {
        match self.review.vcs.tree_diff(base, head) {
            Ok(model) => {
                self.pr_ranges
                    .insert(number, (base.to_owned(), head.to_owned()));
                let source = ReviewSource::pr(number);
                if let Err(err) = self.review.ensure_source(&source) {
                    self.error(err.to_string());
                    return;
                }
                // re-opening the PR already on screen (a head-move refresh,
                // or picking it again from the list) swaps the model in
                // place: the reviewer keeps their cursor, folds, and screen
                // stack instead of landing on a fresh view
                if let Some(diff) = self.diff.as_mut().filter(|d| d.source == source) {
                    diff.commit_model = Some(model);
                    diff.invalidate();
                    diff.ensure_rows(&self.review);
                } else {
                    self.install_diff_view(source, Some(model));
                }
                self.pending_ci = Some(crate::app::CiRequest::PrComments(number));
            }
            Err(err) => self.error(err.to_string()),
        }
    }
}
