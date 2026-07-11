//! CI screens: the runs list, the run graph, and job logs.

use super::{App, CiRequest, MouseGesture, Screen, ci_log, hit_index, page_step};
use crate::keymap::Action;

impl App {
    pub(super) fn ci_log_mouse(&mut self, gesture: MouseGesture) {
        let Some(view) = self.ci_log.as_mut() else {
            return;
        };
        let last = view.rows().len().saturating_sub(1);
        match gesture {
            MouseGesture::Scroll { down, .. } => {
                let delta = if down { 3 } else { -3 };
                view.cursor = view.cursor.saturating_add_signed(delta).min(last);
            }
            MouseGesture::Press { col, row } => {
                if let Some(i) = hit_index(view.body, view.scroll, col, row).filter(|i| *i <= last)
                {
                    view.cursor = i;
                    view.visual_anchor = None;
                }
            }
            MouseGesture::DoublePress { col, row } => {
                if let Some(i) = hit_index(view.body, view.scroll, col, row).filter(|i| *i <= last)
                {
                    view.cursor = i;
                    view.toggle_fold_at_cursor();
                }
            }
            MouseGesture::Drag { col, row } => {
                if let Some(i) = hit_index(view.body, view.scroll, col, row).filter(|i| *i <= last)
                {
                    if view.visual_anchor.is_none() {
                        view.visual_anchor = Some(view.cursor);
                    }
                    view.cursor = i;
                }
            }
            MouseGesture::Cancel => view.visual_anchor = None,
        }
    }

    /// A second left-press at (about) the same cell within the double-click
    /// window. Resets after firing so a third press starts fresh.
    pub(super) fn open_runs(&mut self) {
        if self.ci_remotes.is_empty() {
            self.info("no CI provider detected for this repo");
            return;
        }
        self.runs_cursor = 0;
        self.push_screen(Screen::Runs);
        self.pending_ci = Some(CiRequest::Runs);
    }

    /// Open the selected run's graph: fetch its detail, which arrives as
    /// `AppEvent::CiRunDetail` and feeds the graph view.
    pub(super) fn open_selected_run(&mut self) {
        let Some(run) = self.runs.get(self.runs_cursor) else {
            return;
        };
        let id = run.id.clone();
        self.open_run_remote = run.remote.clone();
        self.open_run = Some(id.clone());
        self.extras = None;
        self.graph = Some(crate::graph::GraphView::new());
        self.push_screen(Screen::Graph);
        self.pending_ci = Some(CiRequest::Detail(id));
    }

    /// Append a job-log chunk, refresh the step metadata, and rebuild the
    /// foldable view, carrying the prior fold state across the re-poll.
    pub(super) fn on_ci_log(
        &mut self,
        text: &str,
        steps: Vec<crate::ci::LogStepMeta>,
        offset: u64,
        done: bool,
    ) {
        self.log_text.push_str(text);
        self.log_offset = offset;
        self.log_done = done;
        if !steps.is_empty() {
            self.log_steps = steps;
        }
        let rebuilt = ci_log::CiLogView::parse(&self.log_text, &self.log_steps);
        self.ci_log = Some(match self.ci_log.take() {
            Some(prev) => prev.carry_into(rebuilt),
            None => rebuilt,
        });
    }

    /// Fold a branch-scoped runs poll into the inline section, then resolve the
    /// branch's PR once (not every poll) via the single `pending_ci` slot.
    pub(super) fn on_ci_runs(&mut self, runs: Vec<crate::ci::CiRun>) {
        self.runs = runs;
        self.runs_cursor = self.runs_cursor.min(self.runs.len().saturating_sub(1));
        // the inline Status section grew/shrank; keep the row cursor valid
        self.clamp_cursor();
        if !self.pr_checked {
            self.pending_ci = Some(CiRequest::Pr);
        }
    }

    /// Fold a run's detail into the graph, then queue its extras once: a single
    /// `pending_ci` slot means the extras request only displaces a run-detail
    /// poll until the extras land, after which the poll keeps the slot.
    pub(super) fn on_run_detail(&mut self, detail: &crate::ci::RunDetail) {
        let model = crate::ci::to_model(detail);
        if let Some(graph) = self.graph.as_mut() {
            graph.set_model(model);
        }
        if self.extras.is_none()
            && self.screen() == Screen::Graph
            && let Some(run) = self.open_run.clone()
        {
            self.pending_ci = Some(CiRequest::Extras(run));
        }
    }

    /// Open a job's log view from a graph node activation.
    pub(super) fn open_ci_log(&mut self, job: crate::ci::JobId) {
        let Some(run) = self.open_run.clone() else {
            return;
        };
        self.open_job = Some(job.clone());
        self.log_text.clear();
        self.log_offset = 0;
        self.log_steps.clear();
        self.ci_log = None;
        self.log_done = false;
        self.push_screen(Screen::CiLog);
        self.pending_ci = Some(CiRequest::Log {
            run,
            job,
            offset: 0,
        });
    }

    /// Queue the poll for the active CI screen onto `pending_ci`.
    pub(super) fn queue_ci_poll(&mut self) {
        self.pending_ci = match self.screen() {
            // the Status screen shows an inline CI-runs section, kept live
            Screen::Status | Screen::Runs => Some(CiRequest::Runs),
            Screen::Graph => self.open_run.clone().map(CiRequest::Detail),
            // stop once the log is complete (a dump provider sends it all at once)
            Screen::CiLog if self.log_done => None,
            // a PR diff re-syncs its forge comments on the same cadence
            Screen::Diff => match self.diff.as_ref().map(|d| &d.source) {
                Some(diffler_core::source::ReviewSource::Pr { number }) => {
                    Some(CiRequest::PrComments(*number))
                }
                _ => None,
            },
            Screen::CiLog => match (self.open_run.clone(), self.open_job.clone()) {
                (Some(run), Some(job)) => Some(CiRequest::Log {
                    run,
                    job,
                    offset: self.log_offset,
                }),
                _ => None,
            },
            Screen::Log | Screen::Prs => None,
        };
    }

    /// While the runs screen is up: navigate the list, Enter opens a run.
    /// The runs list from keymap actions: standard list motions plus Enter.
    pub(super) fn dispatch_runs(&mut self, action: Action) {
        let last = self.runs.len().saturating_sub(1);
        match action {
            Action::MoveDown => self.runs_cursor = (self.runs_cursor + 1).min(last),
            Action::MoveUp => self.runs_cursor = self.runs_cursor.saturating_sub(1),
            Action::GoTop => self.runs_cursor = 0,
            Action::GoBottom => self.runs_cursor = last,
            Action::HalfPageDown => {
                self.runs_cursor = (self.runs_cursor + page_step(0, false)).min(last);
            }
            Action::HalfPageUp => {
                self.runs_cursor = self.runs_cursor.saturating_sub(page_step(0, false));
            }
            Action::FullPageDown => {
                self.runs_cursor = (self.runs_cursor + page_step(0, true)).min(last);
            }
            Action::FullPageUp => {
                self.runs_cursor = self.runs_cursor.saturating_sub(page_step(0, true));
            }
            Action::Open => self.open_selected_run(),
            _ => {}
        }
    }

    /// Drive the foldable CI-log view from a keymap [`Action`]: motions, fold,
    /// visual select, and yank. The `CiLog` screen reuses the diff/log keymap.
    pub(super) fn dispatch_ci_log(&mut self, action: Action) {
        let Some(view) = self.ci_log.as_mut() else {
            return;
        };
        let last = view.rows().len().saturating_sub(1);
        match action {
            Action::MoveDown => view.cursor = (view.cursor + 1).min(last),
            Action::MoveUp => view.cursor = view.cursor.saturating_sub(1),
            Action::GoTop => view.cursor = 0,
            Action::GoBottom => view.cursor = last,
            Action::HalfPageDown => self.ci_log_page(false, false),
            Action::HalfPageUp => self.ci_log_page(true, false),
            Action::FullPageDown => self.ci_log_page(false, true),
            Action::FullPageUp => self.ci_log_page(true, true),
            Action::ToggleFold => view.toggle_fold_at_cursor(),
            Action::VisualSelect => {
                view.visual_anchor = match view.visual_anchor {
                    Some(_) => None,
                    None => Some(view.cursor),
                };
            }
            Action::CopyFileFeedback | Action::CopyAllFeedback => {
                self.pending_clipboard = Some(view.selection_text());
                let view = self.ci_log.as_mut();
                if let Some(view) = view {
                    view.visual_anchor = None;
                }
                self.info("yanked log selection");
            }
            _ => {}
        }
    }

    /// Half/full-page cursor jump over the CI-log view, mirroring `log_page`.
    pub(super) fn ci_log_page(&mut self, up: bool, full: bool) {
        let Some(view) = self.ci_log.as_mut() else {
            return;
        };
        let last = view.rows().len().saturating_sub(1);
        let step = page_step(view.viewport, full);
        view.cursor = if up {
            view.cursor.saturating_sub(step)
        } else {
            (view.cursor + step).min(last)
        };
    }

    /// Keymap actions on the graph screen. Search, help, and back are handled
    /// by the shared dispatch; `n`/`N` arrive as SearchNext/Prev and fall back
    /// to edge-follow there when no search is up.
    pub(super) fn dispatch_graph(&mut self, action: Action) {
        use crate::graph::Dir;
        let Some(graph) = self.graph.as_mut() else {
            return;
        };
        match action {
            Action::MoveDown => graph.move_selection(Dir::Down),
            Action::MoveUp => graph.move_selection(Dir::Up),
            Action::MoveLeft => graph.move_selection(Dir::Left),
            Action::MoveRight => graph.move_selection(Dir::Right),
            Action::GoTop => graph.select_end(true),
            Action::GoBottom => graph.select_end(false),
            Action::ZoomIn => {
                let zoom = graph.zoom().in_();
                graph.set_zoom(zoom);
            }
            Action::ZoomOut => {
                let zoom = graph.zoom().out();
                graph.set_zoom(zoom);
            }
            Action::Open => {
                if let Some(action) = graph.activate() {
                    self.on_graph_action(&action);
                }
            }
            _ => {}
        }
        // folds and zooms relayout the placements the committed match rows
        // index into; recompute so highlights and n/N track the live nodes
        if self.search.is_some() {
            let rows = self.focused_search_rows();
            if let Some(search) = self.search.as_mut() {
                search.recompute(&rows);
            }
        }
    }

    /// React to a [`crate::graph::GraphAction`] from the component: activating a
    /// node opens that job's log.
    pub(super) fn on_graph_action(&mut self, action: &crate::graph::GraphAction) {
        match action {
            crate::graph::GraphAction::Activated(id) => {
                if let Some((path, line)) = self.impact_targets.get(&id.0).cloned() {
                    self.request_editor(&path, Some(line + 1));
                } else if self.impact_title.is_none() {
                    self.open_ci_log(crate::ci::JobId(id.0.clone()));
                }
            }
            crate::graph::GraphAction::Folded { .. } => {}
        }
    }
}
