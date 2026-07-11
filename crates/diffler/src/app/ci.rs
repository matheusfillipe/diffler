//! CI screens: the runs list and the run graph. Job-log handlers live with
//! their screen state in [`super::ci_log`].

use super::{App, CiRequest, Screen, page_step};
use crate::keymap::Action;

impl App {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CiRemote;
    use crate::ci::{CiJob, CiRun, JobId, JobStatus, RunDetail, RunId};
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, standard_fixture};

    fn app() -> (Fixture, App) {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        (fixture, app)
    }

    fn run(id: &str) -> CiRun {
        CiRun {
            id: RunId(id.to_owned()),
            name: "CI".into(),
            title: String::new(),
            branch: "main".into(),
            commit: "abc".into(),
            author: String::new(),
            created: None,
            status: JobStatus::Running,
            url: None,
            remote: None,
        }
    }

    fn github_remote() -> CiRemote {
        CiRemote {
            name: "origin".into(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::GitHub,
                host: None,
            },
            url: None,
        }
    }

    #[test]
    fn open_runs_pushes_the_runs_screen_and_queues_a_poll() {
        let (_fixture, mut app) = app();
        app.ci_remotes = vec![github_remote()];
        app.open_runs();
        assert_eq!(app.screen(), Screen::Runs);
        assert_eq!(app.runs_selected(), 0);
        assert!(matches!(app.pending_ci, Some(CiRequest::Runs)));
    }

    #[test]
    fn open_selected_run_opens_the_graph_and_queues_its_detail() {
        let (_fixture, mut app) = app();
        app.runs = vec![run("42")];
        app.open_selected_run();
        assert_eq!(app.screen(), Screen::Graph);
        assert!(app.graph.is_some());
        match app.pending_ci {
            Some(CiRequest::Detail(RunId(ref id))) => assert_eq!(id, "42"),
            other => panic!("expected a detail poll, got {other:?}"),
        }
    }

    #[test]
    fn open_selected_run_with_no_run_at_the_cursor_is_a_noop() {
        let (_fixture, mut app) = app();
        app.open_selected_run();
        assert_eq!(app.screen(), Screen::Status, "no run: nothing pushed");
        assert!(app.graph.is_none());
        assert!(app.pending_ci.is_none());
    }

    #[test]
    fn on_ci_runs_replaces_the_list_clamps_the_cursor_and_checks_the_pr_once() {
        let (_fixture, mut app) = app();
        app.runs_cursor = 5;
        app.on_ci_runs(vec![run("1"), run("2")]);
        assert_eq!(app.runs.len(), 2);
        assert_eq!(app.runs_selected(), 1, "cursor clamps to the last row");
        assert!(
            matches!(app.pending_ci, Some(CiRequest::Pr)),
            "an unresolved PR queues a check"
        );

        app.pending_ci = None;
        app.pr_checked = true;
        app.on_ci_runs(vec![run("1")]);
        assert!(
            app.pending_ci.is_none(),
            "the PR is already checked for this branch: no re-queue"
        );
    }

    #[test]
    fn queue_ci_poll_on_status_and_runs_polls_the_runs_list() {
        let (_fixture, mut app) = app();
        app.queue_ci_poll();
        assert!(matches!(app.pending_ci, Some(CiRequest::Runs)));

        app.push_screen(Screen::Runs);
        app.pending_ci = None;
        app.queue_ci_poll();
        assert!(matches!(app.pending_ci, Some(CiRequest::Runs)));
    }

    #[test]
    fn queue_ci_poll_on_the_graph_polls_the_open_runs_detail() {
        let (_fixture, mut app) = app();
        app.open_run = Some(RunId("7".into()));
        app.push_screen(Screen::Graph);
        app.queue_ci_poll();
        match app.pending_ci {
            Some(CiRequest::Detail(RunId(ref id))) => assert_eq!(id, "7"),
            other => panic!("expected a detail poll, got {other:?}"),
        }
    }

    #[test]
    fn queue_ci_poll_on_ci_log_stops_once_the_log_is_done() {
        let (_fixture, mut app) = app();
        app.open_run = Some(RunId("7".into()));
        app.open_job = Some(JobId("lint".into()));
        app.log_offset = 12;
        app.push_screen(Screen::CiLog);

        app.log_done = true;
        app.queue_ci_poll();
        assert!(app.pending_ci.is_none(), "a finished log stops polling");

        app.log_done = false;
        app.queue_ci_poll();
        match app.pending_ci {
            Some(CiRequest::Log {
                ref run,
                ref job,
                offset,
            }) => {
                assert_eq!(run.0, "7");
                assert_eq!(job.0, "lint");
                assert_eq!(offset, 12);
            }
            other => panic!("expected a log poll, got {other:?}"),
        }
    }

    #[test]
    fn queue_ci_poll_on_a_pr_diff_polls_its_comments_but_not_a_working_tree_diff() {
        let fixture = standard_fixture();
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    43\n}\n");
        fixture.stage("src/lib.rs");
        fixture.commit_all("bump");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let head = app.review.vcs.resolve("HEAD").expect("head oid");
        let base = app.review.vcs.resolve("HEAD~1").expect("base oid");
        app.open_pr_diff(3, &base, &head);
        app.pending_ci = None;
        app.queue_ci_poll();
        assert!(matches!(app.pending_ci, Some(CiRequest::PrComments(3))));

        app.open_working_tree_diff(None);
        app.pending_ci = None;
        app.queue_ci_poll();
        assert!(app.pending_ci.is_none(), "no forge comments to re-sync");
    }

    #[test]
    fn dispatch_runs_moves_the_cursor_and_opens_the_graph_on_enter() {
        let (_fixture, mut app) = app();
        app.runs = vec![run("1"), run("2"), run("3")];
        app.dispatch_runs(Action::GoBottom);
        assert_eq!(app.runs_selected(), 2);
        app.dispatch_runs(Action::MoveUp);
        assert_eq!(app.runs_selected(), 1);
        app.dispatch_runs(Action::GoTop);
        assert_eq!(app.runs_selected(), 0);
        app.dispatch_runs(Action::Open);
        assert_eq!(app.screen(), Screen::Graph);
    }

    fn graph_app() -> (Fixture, App) {
        let (fixture, mut app) = app();
        app.open_run = Some(RunId("7".into()));
        app.push_screen(Screen::Graph);
        let detail = RunDetail {
            run: run("7"),
            jobs: vec![CiJob {
                id: JobId("lint".into()),
                name: "lint".into(),
                status: JobStatus::Ok,
                needs: vec![],
            }],
        };
        let mut graph = crate::graph::GraphView::new();
        graph.set_model(crate::ci::to_model(&detail));
        app.graph = Some(graph);
        (fixture, app)
    }

    #[test]
    fn dispatch_graph_moves_the_selection_and_zooms() {
        let (_fixture, mut app) = graph_app();
        app.dispatch_graph(Action::GoBottom);
        assert!(app.graph.as_ref().unwrap().selected().is_some());

        let before = app.graph.as_ref().unwrap().zoom();
        app.dispatch_graph(Action::ZoomOut);
        assert_ne!(app.graph.as_ref().unwrap().zoom(), before);
    }

    #[test]
    fn dispatch_graph_open_activates_the_selected_node() {
        let (_fixture, mut app) = graph_app();
        app.dispatch_graph(Action::GoBottom);
        app.dispatch_graph(Action::Open);
        // the lone job has no impact target, so activation opens its log
        assert_eq!(app.screen(), Screen::CiLog);
        assert_eq!(app.open_job_name().as_deref(), Some("lint"));
    }

    #[test]
    fn on_graph_action_with_an_impact_target_opens_the_editor_instead_of_a_log() {
        let (_fixture, mut app) = app();
        app.impact_targets
            .insert("lint".into(), ("src/lib.rs".into(), 4));
        app.on_graph_action(&crate::graph::GraphAction::Activated(
            crate::graph::NodeId::new("lint"),
        ));
        assert_eq!(app.screen(), Screen::Status, "no log screen opened");
        let request = app.pending_editor.expect("editor request queued");
        assert!(matches!(
            request.purpose,
            crate::editor::EditorPurpose::OpenFile { ref path } if path == "src/lib.rs"
        ));
    }

    #[test]
    fn on_graph_action_folded_is_a_noop() {
        let (_fixture, mut app) = app();
        app.on_graph_action(&crate::graph::GraphAction::Folded {
            group: "matrix".into(),
            collapsed: true,
        });
        assert!(app.pending_editor.is_none());
        assert_eq!(app.screen(), Screen::Status);
    }
}
