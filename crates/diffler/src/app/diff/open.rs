//! Opening the diff screen on one review source: the working tree, a commit,
//! a range, a three-dot base, or a pull request.

use diffler_core::model::DiffModel;
use diffler_core::source::ReviewSource;

use super::{DiffView, Pane};
use crate::app::{App, Screen};

pub(crate) enum PrLookup {
    Known(crate::ci::PullRequest),
    /// The forge's open list came back without it.
    NotOpen,
    NoForge,
    Loading,
}

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

    /// Open the walkthrough `id` as its own review source, rendering whatever
    /// review it is about: the working tree by default, or the commit,
    /// range, or PR the human had open when it was published. It opens over
    /// an empty diff too, since its anchored files fill the pane once they
    /// resolve. Returns `false` while a PR's range is still resolving, leaving
    /// `self.diff` untouched for the caller to retry.
    pub(crate) fn open_walkthrough_diff(&mut self, id: &str) -> bool {
        let about = self.walkthrough_about(id);
        if let ReviewSource::Pr { number } = about
            && !self.pr_ranges.contains_key(&number)
            && !self.resolve_walkthrough_pr(number)
        {
            return false;
        }
        let model = match about {
            ReviewSource::WorkingTree => None,
            other => Some((*self.source_model(&other)).clone()),
        };
        self.install_diff_view(ReviewSource::Walkthrough { id: id.to_owned() }, model, true);
        true
    }

    fn open_working_tree_diff_focused(&mut self, scope: Option<&str>, focus: Pane) {
        self.install_diff_view(ReviewSource::WorkingTree, None, false);
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

    /// Load the source's review state, install a fresh `DiffView` and push the
    /// diff screen. A load failure is reported and changes nothing.
    /// `allow_empty` skips the empty-diff refusal, for a walkthrough whose own
    /// files fill the pane once they resolve.
    fn install_diff_view(
        &mut self,
        source: ReviewSource,
        model: Option<DiffModel>,
        allow_empty: bool,
    ) {
        // we refuse to open a source with no files and say why; a review
        // already open stays open when its diff empties out
        let files = model.as_ref().map_or_else(
            || self.review.model().files.len(),
            |model| model.files.len(),
        );
        if files == 0 && !allow_empty {
            self.info(if source == ReviewSource::WorkingTree {
                "nothing to review: working tree clean".to_owned()
            } else {
                format!("nothing to review in {}", source.label())
            });
            return;
        }
        if let Err(err) = self.review.ensure_source(&source) {
            self.error(err.to_string());
            return;
        }
        // a queued open can arrive while a comment is being written, so we
        // carry the draft over when it still belongs here
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
            self.config.classify.rules(),
            self.config.ui.side_by_side,
        );
        match draft {
            Some(draft) if same_source => {
                // the composer only draws on the file it is anchored to, so
                // we select that file in the rebuilt view
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
        self.queue_declared();
        self.ensure_walkthrough_view();
        self.push_screen(Screen::Diff);
    }

    pub(crate) fn open_commit_diff(&mut self, oid: &str) {
        match self.review.vcs.commit_diff(oid) {
            Ok(model) => self.install_diff_view(ReviewSource::commit(oid), Some(model), false),
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
                if let Some(diff) = self.diff.as_ref().filter(|d| d.source == source) {
                    // capture before the model swap, or the position named
                    // would already read against the row it is moving to
                    let positions = diff.capture_positions(&self.review);
                    self.finish_diff_swap(positions, Some(model));
                } else {
                    self.install_diff_view(source, Some(model), false);
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
            Ok(model) => {
                self.install_diff_view(ReviewSource::range(oldest, newest), Some(model), false);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    /// Review the branch's open PR: diff `merge-base..head` under the stable
    /// `pr-<n>` source. A head we don't have yet is fetched first, from the
    /// ref the forge serves it under, and the open retries after the fetch.
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
        let number = pr.number;
        if let Some((base, head)) = self.ensure_pr_range(pr) {
            self.open_pr_diff(number, &base, &head);
        }
    }

    /// `(merge_base, head)` for `pr` against the local objects, fetching its
    /// head first when the repository lacks it. `None` means the caller
    /// retries from the `git_finished` continuation keyed off
    /// `pending_pr_open`.
    pub(crate) fn ensure_pr_range(
        &mut self,
        pr: crate::ci::PullRequest,
    ) -> Option<(String, String)> {
        if let Some(range) = self.resolve_pr_range(&pr) {
            return Some(range);
        }
        let (remote, refspec) = self.pr_head_source(pr.number);
        let base_ref = pr.base_ref.clone();
        let label = Self::pr_fetch_label(pr.number);
        self.pending_pr_open = Some(pr);
        // we fetch the base ref too so merge-base matches the forge's view
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
        None
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

    /// The PR the branch's own current review or the fetched open-PRs list
    /// already knows about, if either names `number`.
    pub(crate) fn known_pr(&self, number: u64) -> Option<crate::ci::PullRequest> {
        self.pr
            .clone()
            .filter(|pr| pr.number == number)
            .or_else(|| self.prs.iter().find(|pr| pr.number == number).cloned())
    }

    /// The PR `number` names among the ones this session knows, asking the
    /// forge for the open list when it has not answered yet.
    pub(crate) fn find_pr(&mut self, number: u64) -> PrLookup {
        if let Some(pr) = self.known_pr(number) {
            return PrLookup::Known(pr);
        }
        if self.status.prs_loaded {
            return PrLookup::NotOpen;
        }
        if self.ci_remotes.is_empty() {
            return PrLookup::NoForge;
        }
        self.request_pr_list();
        PrLookup::Loading
    }

    pub(crate) fn request_pr_list(&mut self) {
        if !self.status.prs_in_flight {
            self.status.prs_in_flight = true;
            self.pending_ci = Some(crate::app::CiRequest::Prs);
        }
    }

    /// Make sure `pr_ranges` holds `number`, queueing a head fetch or the
    /// open-PRs list when needed. `false` means resolution is still in flight
    /// and the caller retries once it completes.
    pub(crate) fn resolve_walkthrough_pr(&mut self, number: u64) -> bool {
        let pr = match self.find_pr(number) {
            PrLookup::Known(pr) => pr,
            PrLookup::NotOpen => {
                self.error(format!(
                    "PR #{number} isn't among the repo's open pull requests; \
                     this walkthrough's diff can't be resolved"
                ));
                return false;
            }
            PrLookup::NoForge => {
                self.error(format!(
                    "no CI provider detected for this repo; \
                     can't resolve PR #{number} for this walkthrough"
                ));
                return false;
            }
            PrLookup::Loading => {
                self.info(format!("loading PR #{number} for this walkthrough"));
                return false;
            }
        };
        if let Some((base, head)) = self.ensure_pr_range(pr) {
            self.pr_ranges.insert(number, (base, head));
            true
        } else {
            self.info(format!("fetching PR #{number} for this walkthrough"));
            false
        }
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
                // re-opening the PR already on screen swaps the model in place,
                // so the reviewer keeps their cursor, folds and screen stack
                if let Some(diff) = self.diff.as_ref().filter(|d| d.source == source) {
                    // capture before the model swap, or the position named
                    // would already read against the row it is moving to
                    let positions = diff.capture_positions(&self.review);
                    self.finish_diff_swap(positions, Some(model));
                } else {
                    self.install_diff_view(source, Some(model), false);
                }
                self.pending_ci = Some(crate::app::CiRequest::PrComments(number));
            }
            Err(err) => self.error(err.to_string()),
        }
    }
}
