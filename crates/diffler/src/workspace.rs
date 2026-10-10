//! Project tabs: each tab is a whole [`App`] over its own repository. The
//! workspace routes input to the tab on screen, worker answers to the tab that
//! asked, and agent calls to the tab they name.

use std::path::{Path, PathBuf};

use diffler_core::review::Review;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::app::tabs::{TabOp, TabStrip, expand_home, nearby_repos};
use crate::app::{App, Flow};
use crate::config::{self, CliOverrides};
use crate::editor::{EditorPurpose, EditorRequest};
use crate::event::AppEvent;
use crate::keymap::{self, Resolved};
use crate::mcp::{self, FocusTarget, McpRequest, McpRequestKind, McpResponse};

#[derive(Debug)]
pub enum WsEvent {
    /// Terminal input and ticks.
    Input(AppEvent),
    /// A worker or watcher answer for the tab with this id.
    Tab(u64, AppEvent),
    Mcp(AppEvent),
}

/// Tags each event source for the workspace. The forwarder ends once every
/// sender of the returned channel is gone.
pub fn forward(
    main: &mpsc::UnboundedSender<WsEvent>,
    wrap: impl Fn(AppEvent) -> WsEvent + Send + 'static,
) -> (mpsc::UnboundedSender<AppEvent>, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let main = main.clone();
    let handle = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if main.send(wrap(event)).is_err() {
                break;
            }
        }
    });
    (tx, handle)
}

pub struct Tab {
    pub id: u64,
    pub app: App,
    pub tx: mpsc::UnboundedSender<AppEvent>,
    /// Canonicalized, so two spellings of one folder compare equal.
    root: PathBuf,
    _watcher: Option<crate::watch::WatcherHandle>,
    forward: JoinHandle<()>,
    /// The tab's own feedback epoch when the workspace last looked.
    seen_epoch: u64,
}

impl Tab {
    fn clear_endpoint(&self, port: Option<u16>) {
        if let Some(port) = port {
            mcp::clear_endpoint(&self.app.review.repo_root, port);
        }
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.forward.abort();
    }
}

pub struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    main_tx: mpsc::UnboundedSender<WsEvent>,
    overrides: CliOverrides,
    /// Moves whenever any tab's own epoch does, and never goes back when a tab closes.
    feedback_tx: watch::Sender<u64>,
    mcp_port: Option<u16>,
    /// Whether the terminal has focus; only the tab in front shares it.
    focused: bool,
}

impl Workspace {
    /// Must run inside the tokio runtime, since every tab spawns a forwarder.
    pub fn new(app: App, main_tx: mpsc::UnboundedSender<WsEvent>, overrides: CliOverrides) -> Self {
        let mut workspace = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            main_tx,
            overrides,
            feedback_tx: watch::Sender::new(0),
            mcp_port: None,
            focused: true,
        };
        workspace.push_tab(app);
        workspace
    }

    // we never close the last tab and clamp `active` on every close, so it
    // always names an open tab
    #[allow(clippy::expect_used)]
    pub fn active(&self) -> &App {
        &self
            .tabs
            .get(self.active)
            .expect("the workspace keeps its active index on an open tab")
            .app
    }

    // the same invariant as `active`
    #[allow(clippy::expect_used)]
    pub fn active_mut(&mut self) -> &mut App {
        &mut self
            .tabs
            .get_mut(self.active)
            .expect("the workspace keeps its active index on an open tab")
            .app
    }

    /// Only the tab in front asks for a redraw.
    fn handle_in(&mut self, index: usize, event: AppEvent) -> Flow {
        let Some(tab) = self.tabs.get_mut(index) else {
            return Flow::Idle;
        };
        let flow = tab.app.handle(event);
        if index == self.active {
            flow
        } else {
            Flow::Idle
        }
    }

    pub fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.tabs.iter_mut()
    }

    pub fn feedback_rx(&self) -> watch::Receiver<u64> {
        self.feedback_tx.subscribe()
    }

    /// A tab switch can happen between the request and the main loop's next
    /// look, so we take it from whichever tab left it.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.tabs
            .iter_mut()
            .find_map(|tab| tab.app.pending_clipboard.take())
    }

    pub fn take_editor(&mut self) -> Option<(u64, EditorRequest)> {
        self.tabs
            .iter_mut()
            .find_map(|tab| Some((tab.id, tab.app.pending_editor.take()?)))
    }

    pub fn editor_finished(
        &mut self,
        id: u64,
        purpose: EditorPurpose,
        outcome: Result<bool, String>,
    ) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.app.editor_finished(purpose, outcome);
        }
    }

    /// The main loop serves these before draining more events.
    pub fn has_pending(&self) -> bool {
        self.tabs.iter().any(|tab| {
            let app = &tab.app;
            app.pending_editor.is_some()
                || app.pending_git.is_some()
                || app.pending_ci.is_some()
                || app.pending_clipboard.is_some()
        })
    }

    pub fn set_mcp_port(&mut self, port: u16) {
        self.mcp_port = Some(port);
        for tab in &mut self.tabs {
            publish_endpoint(&mut tab.app, port);
        }
    }

    pub fn clear_endpoints(&self) {
        for tab in &self.tabs {
            tab.clear_endpoint(self.mcp_port);
        }
    }

    fn push_tab(&mut self, mut app: App) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, forward) = forward(&self.main_tx, move |event| WsEvent::Tab(id, event));
        let git_dir = app
            .review
            .vcs
            .git_dir()
            .unwrap_or_else(|_| app.review.repo_root.join(".git"));
        // without a watcher the app falls back to periodic polling
        let watcher = crate::watch::spawn_watcher(&app.review.repo_root, &git_dir, tx.clone()).ok();
        app.watcher_healthy = watcher.as_ref().map(|handle| handle.healthy.clone());
        if let Some(port) = self.mcp_port {
            publish_endpoint(&mut app, port);
        }
        if let Some(first) = self.tabs.first() {
            app.image_picker = first.app.image_picker.clone();
        }
        let seen_epoch = app.feedback_epoch();
        let root = mcp::canonical_repo(&app.review.repo_root);
        self.tabs.push(Tab {
            id,
            app,
            tx,
            root,
            _watcher: watcher,
            forward,
            seen_epoch,
        });
        self.tabs.len() - 1
    }

    /// Reuses the tab already showing `path`.
    pub fn open(&mut self, path: &Path) -> Result<usize, String> {
        let root = diffler_core::repo::discover(path).map_err(|err| err.to_string())?;
        let canonical = mcp::canonical_repo(&root);
        if let Some(index) = self.tabs.iter().position(|tab| tab.root == canonical) {
            return Ok(index);
        }
        let loaded = config::load(Some(&root), &self.overrides).map_err(|err| err.to_string())?;
        let review = Review::open_with_settings(&root, &loaded.config.diff_settings())
            .map_err(|err| err.to_string())?;
        Ok(self.push_tab(App::new(review, loaded)))
    }

    pub fn handle(&mut self, event: WsEvent) -> Flow {
        let flow = match event {
            WsEvent::Input(AppEvent::Quit) => return Flow::Quit,
            WsEvent::Input(AppEvent::Key(key)) => self.handle_key(key),
            WsEvent::Input(AppEvent::Focus(focused)) => {
                self.focused = focused;
                self.active_mut().handle(AppEvent::Focus(focused))
            }
            WsEvent::Input(event @ (AppEvent::Tick | AppEvent::Resize))
            | WsEvent::Mcp(event @ AppEvent::McpWaiting { .. }) => self.broadcast(&event),
            WsEvent::Tab(id, event) => match self.tabs.iter().position(|tab| tab.id == id) {
                Some(index) => self.handle_in(index, event),
                None => Flow::Idle,
            },
            WsEvent::Mcp(AppEvent::Mcp(request)) => self.handle_mcp(request),
            WsEvent::Input(event) | WsEvent::Mcp(event) => self.active_mut().handle(event),
        };
        let tab_flow = self.apply_tab_op();
        self.publish_feedback();
        match (flow, tab_flow) {
            (Flow::Quit, _) => Flow::Quit,
            (Flow::Continue, _) | (_, Flow::Continue) => Flow::Continue,
            _ => Flow::Idle,
        }
    }

    /// Only the tab in front decides the redraw.
    fn broadcast(&mut self, event: &AppEvent) -> Flow {
        let mut flow = Flow::Idle;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let Some(copy) = copy_event(event) else {
                continue;
            };
            let answer = tab.app.handle(copy);
            if index == self.active {
                flow = answer;
            }
        }
        flow
    }

    /// Tab keys resolve before the app sees the key, so a tab switch works
    /// from any screen, over a dialog or a draft too.
    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Flow {
        let press = keymap::press_from_event(&key);
        if let Resolved::Action(action) =
            self.active().tabs_keymap().resolve(&mut Vec::new(), press)
            && crate::app::tabs::tab_op(action).is_some()
        {
            self.active_mut().request_tab(action);
            return Flow::Continue;
        }
        self.active_mut().handle(AppEvent::Key(key))
    }

    fn apply_tab_op(&mut self) -> Flow {
        let Some(op) = self.active_mut().pending_tab.take() else {
            return Flow::Idle;
        };
        let count = self.tabs.len();
        let before = self.active;
        match op {
            TabOp::Next => self.active = (self.active + 1) % count,
            TabOp::Prev => self.active = (self.active + count - 1) % count,
            TabOp::Go(index) if index < count => self.active = index,
            TabOp::Go(index) => self
                .active_mut()
                .info(format!("open more projects to use tab {}", index + 1)),
            TabOp::Close if count == 1 => self
                .active_mut()
                .info("add another project before closing this one"),
            TabOp::Close => {
                let tab = self.tabs.remove(self.active);
                tab.clear_endpoint(self.mcp_port);
                self.active = self.active.min(self.tabs.len() - 1);
            }
            TabOp::Pick => {
                let roots: Vec<PathBuf> = self.tabs.iter().map(|tab| tab.root.clone()).collect();
                self.active_mut().open_project_picker(nearby_repos(&roots));
            }
            TabOp::Open(path) => match self.open(&path) {
                Ok(index) => self.active = index,
                Err(err) => self.active_mut().error(err),
            },
        }
        if self.active != before {
            self.hand_over_focus(before);
        }
        Flow::Continue
    }

    fn activate(&mut self, index: usize) {
        let left = self.active;
        self.active = index;
        if left != index {
            self.hand_over_focus(left);
        }
    }

    /// A background tab polls the way an unfocused one does.
    fn hand_over_focus(&mut self, left: usize) {
        if let Some(tab) = self.tabs.get_mut(left) {
            tab.app.handle(AppEvent::Focus(false));
        }
        let focused = self.focused;
        self.active_mut().handle(AppEvent::Focus(focused));
    }

    pub fn sync_strip(&mut self) {
        let names: Vec<String> = self.tabs.iter().map(|tab| tab.app.project_name()).collect();
        let strip = (names.len() > 1).then_some(TabStrip {
            names,
            active: self.active,
        });
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            tab.app.tab_strip = if index == self.active {
                strip.clone()
            } else {
                None
            };
        }
    }

    fn publish_feedback(&mut self) {
        let mut moved = false;
        for tab in &mut self.tabs {
            let epoch = tab.app.feedback_epoch();
            if epoch != tab.seen_epoch {
                tab.seen_epoch = epoch;
                moved = true;
            }
        }
        if moved {
            self.feedback_tx.send_modify(|epoch| *epoch += 1);
        }
    }

    fn handle_mcp(&mut self, request: McpRequest) -> Flow {
        let McpRequest {
            kind,
            project,
            reply,
        } = request;
        if reply.is_closed() {
            return Flow::Idle;
        }
        let response = match kind {
            McpRequestKind::OpenProject { path } => self.agent_open_project(&path),
            McpRequestKind::Focus { .. } => match self.target_tab(&kind, project.as_deref()) {
                Ok(index) => self.agent_focus_in(index, kind),
                Err(err) => McpResponse::Error(err),
            },
            McpRequestKind::ReviewStatus => self.status_across_tabs(),
            McpRequestKind::GetComments { .. }
            | McpRequestKind::Feedback
            | McpRequestKind::ListReviews => self.merge_across_tabs(&kind),
            kind => {
                let index = match self.target_tab(&kind, project.as_deref()) {
                    Ok(index) => index,
                    Err(err) => {
                        let _ = reply.send(McpResponse::Error(err));
                        return Flow::Idle;
                    }
                };
                // the visible status bar shows the agent's activity whichever tab the call went to
                if index != self.active {
                    self.active_mut().record_mcp_activity(&kind);
                }
                let request = McpRequest {
                    kind,
                    project: None,
                    reply,
                };
                let flow = self.handle_in(index, AppEvent::Mcp(request));
                return if index == self.active {
                    flow
                } else {
                    Flow::Continue
                };
            }
        };
        let _ = reply.send(response);
        Flow::Continue
    }

    /// The tab owning the id the call names, else the named project, else the tab in front.
    fn target_tab(&self, kind: &McpRequestKind, project: Option<&str>) -> Result<usize, String> {
        let id = match kind {
            McpRequestKind::ReplyComment { id, .. }
            | McpRequestKind::ProposeResolve { id, .. }
            | McpRequestKind::DeleteComment { id }
            | McpRequestKind::EditComment { id, .. }
            | McpRequestKind::GetWalkthrough { id: Some(id) }
            | McpRequestKind::PublishWalkthrough { id: Some(id), .. }
            | McpRequestKind::Focus {
                target: FocusTarget::Id(id),
                ..
            } => Some(id.as_str()),
            _ => None,
        };
        if let Some(id) = id
            && self.tabs.len() > 1
            && let Some(index) = self.owner_of(id)
        {
            return Ok(index);
        }
        let Some(project) = project else {
            return Ok(self.active);
        };
        let path = mcp::canonical_repo(&expand_home(project));
        let named: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.root == path || tab.app.project_name() == project)
            .map(|(index, _)| index)
            .collect();
        match named.as_slice() {
            [index] => Ok(*index),
            [] => Err(format!(
                "no open project named {project}; open it with open_project"
            )),
            _ => Err(format!(
                "several open projects are named {project}; pass its path as project"
            )),
        }
    }

    /// Asks the tab in front first, since the agent mostly answers what the human is reading.
    fn owner_of(&self, id: &str) -> Option<usize> {
        let others = (0..self.tabs.len()).filter(|index| *index != self.active);
        std::iter::once(self.active)
            .chain(others)
            .find(|index| self.tabs.get(*index).is_some_and(|tab| tab.app.owns_id(id)))
    }

    fn status_across_tabs(&mut self) -> McpResponse {
        let active = self.active;
        let epoch = *self.feedback_tx.borrow();
        let projects = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| tab.app.project_info(index == active))
            .collect();
        match self.active_mut().handle_mcp(McpRequestKind::ReviewStatus) {
            McpResponse::Status(mut status) => {
                status.projects = projects;
                status.feedback_epoch = epoch;
                McpResponse::Status(status)
            }
            other => other,
        }
    }

    /// Each item names its project once more than one is open.
    fn merge_across_tabs(&mut self, kind: &McpRequestKind) -> McpResponse {
        let tagged = self.tabs.len() > 1;
        let mut comments = Vec::new();
        let mut reviews = Vec::new();
        for tab in &mut self.tabs {
            let project = tagged.then(|| tab.app.project_name());
            match tab.app.handle_mcp(kind.clone()) {
                McpResponse::Comments(found) | McpResponse::Feedback { comments: found } => {
                    comments.extend(found.into_iter().map(|mut comment| {
                        comment.project.clone_from(&project);
                        comment
                    }));
                }
                McpResponse::Reviews(found) => {
                    reviews.extend(found.into_iter().map(|mut review| {
                        review.project.clone_from(&project);
                        review
                    }));
                }
                error @ McpResponse::Error(_) => return error,
                _ => {}
            }
        }
        match kind {
            McpRequestKind::Feedback => McpResponse::Feedback { comments },
            McpRequestKind::ListReviews => McpResponse::Reviews(reviews),
            _ => McpResponse::Comments(comments),
        }
    }

    /// Moves the human to the tab the focus lands in. A draft in the tab in
    /// front keeps the human there, since switching would hide it.
    fn agent_focus_in(&mut self, index: usize, kind: McpRequestKind) -> McpResponse {
        if index != self.active && self.active().busy_typing() {
            return McpResponse::Error(crate::app::TYPING_REFUSAL.to_owned());
        }
        let Some(tab) = self.tabs.get_mut(index) else {
            return McpResponse::Error("no such project tab".to_owned());
        };
        let response = tab.app.handle_mcp(kind);
        if !matches!(response, McpResponse::Error(_)) {
            self.activate(index);
        }
        response
    }

    /// Opens behind the human's own tab.
    fn agent_open_project(&mut self, path: &str) -> McpResponse {
        let count = self.tabs.len();
        match self.open(&expand_home(path)) {
            Ok(index) => {
                let active = self.active;
                let Some(info) = self
                    .tabs
                    .get(index)
                    .map(|tab| tab.app.project_info(index == active))
                else {
                    return McpResponse::Error(format!("no project at {path}"));
                };
                if self.tabs.len() > count {
                    let name = info.name.clone();
                    self.active_mut()
                        .info(format!("agent opened {name} as tab {}", index + 1));
                }
                McpResponse::ProjectOpened(info)
            }
            Err(err) => McpResponse::Error(err),
        }
    }
}

fn copy_event(event: &AppEvent) -> Option<AppEvent> {
    match event {
        AppEvent::Tick => Some(AppEvent::Tick),
        AppEvent::Resize => Some(AppEvent::Resize),
        AppEvent::McpWaiting { until } => Some(AppEvent::McpWaiting { until: *until }),
        _ => None,
    }
}

/// So a proxy started in this project finds this diffler.
fn publish_endpoint(app: &mut App, port: u16) {
    app.mcp_port = Some(port);
    if let Err(err) = mcp::write_endpoint(&app.review.repo_root, port) {
        app.error(format!("failed to write mcp endpoint file: {err}"));
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use tokio::sync::oneshot;

    use super::*;
    use crate::app::Screen;
    use crate::config::LoadedConfig;
    use crate::test_support::Fixture;

    /// A repository with one changed line, ready for a comment on line 1.
    fn changed_repo() -> Fixture {
        let fixture = Fixture::new();
        fixture.write("a.txt", "one\n");
        fixture.commit_all("base");
        fixture.write("a.txt", "two\n");
        fixture
    }

    fn workspace(first: &Fixture) -> (Workspace, mpsc::UnboundedReceiver<WsEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = App::new(first.review(), LoadedConfig::default());
        (Workspace::new(app, tx, CliOverrides::default()), rx)
    }

    fn canon(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).expect("a real folder")
    }

    fn root(workspace: &Workspace) -> PathBuf {
        canon(&workspace.active().review.repo_root)
    }

    fn alt(c: char) -> WsEvent {
        WsEvent::Input(AppEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::ALT,
        )))
    }

    fn call(
        workspace: &mut Workspace,
        project: Option<&Path>,
        kind: McpRequestKind,
    ) -> McpResponse {
        let (reply, mut rx) = oneshot::channel();
        workspace.handle(WsEvent::Mcp(AppEvent::Mcp(McpRequest {
            kind,
            project: project.map(|path| path.display().to_string()),
            reply,
        })));
        rx.try_recv().expect("an answer")
    }

    fn comment_on(workspace: &mut Workspace, project: &Path, body: &str) -> String {
        let kind = McpRequestKind::AddComment {
            file: "a.txt".to_owned(),
            line: 1,
            line_end: None,
            body: body.to_owned(),
            as_human: true,
        };
        match call(workspace, Some(project), kind) {
            McpResponse::Added { id } => id,
            other => panic!("the comment was refused: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tab_keys_switch_projects_and_each_keeps_its_own_screen() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.open(&second.root).expect("second project");
        assert_eq!(
            root(&workspace),
            canon(&first.root),
            "opening by path stays put"
        );

        workspace.active_mut().open_working_tree_diff(None);
        workspace.handle(alt('l'));
        assert_eq!(root(&workspace), canon(&second.root));
        assert_eq!(workspace.active().screen(), Screen::Status);

        workspace.handle(alt('1'));
        assert_eq!(root(&workspace), canon(&first.root));
        assert_eq!(
            workspace.active().screen(),
            Screen::Diff,
            "back where it was left"
        );

        workspace.sync_strip();
        let strip = workspace.active().tab_strip.clone().expect("a tab row");
        assert_eq!((strip.names.len(), strip.active), (2, 0));
    }

    #[tokio::test]
    async fn the_agent_reads_every_projects_comments_and_replies_where_each_lives() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.open(&second.root).expect("second project");
        comment_on(&mut workspace, &first.root, "in the first");
        let id = comment_on(&mut workspace, &second.root, "in the second");

        let McpResponse::Comments(comments) = call(
            &mut workspace,
            None,
            McpRequestKind::GetComments { status: None },
        ) else {
            panic!("comments");
        };
        assert_eq!(comments.len(), 2, "both projects");
        assert!(comments.iter().all(|comment| comment.project.is_some()));

        let reply = McpRequestKind::ReplyComment {
            id: id.clone(),
            body: "answered".to_owned(),
        };
        assert!(matches!(
            call(&mut workspace, None, reply),
            McpResponse::Replied { .. }
        ));
        assert!(
            workspace.tabs[1].app.owns_id(&id),
            "the reply found the second project"
        );
    }

    #[tokio::test]
    async fn focusing_a_comment_brings_its_project_to_the_front_on_its_card() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.open(&second.root).expect("second project");
        let id = comment_on(&mut workspace, &second.root, "look at this");
        assert_eq!(root(&workspace), canon(&first.root));

        let focus = McpRequestKind::Focus {
            target: FocusTarget::Id(id.clone()),
            note: None,
        };
        let McpResponse::Focused(focused) = call(&mut workspace, None, focus) else {
            panic!("the focus was refused");
        };
        assert_eq!(focused.view, crate::mcp::FocusView::Diff);
        assert_eq!(root(&workspace), canon(&second.root), "the tab switched");
        assert_eq!(workspace.active().screen(), Screen::Diff);
        assert!(
            workspace.tabs[0].app.diff.is_none(),
            "the first tab stayed put"
        );
    }

    #[tokio::test]
    async fn a_draft_in_the_tab_in_front_keeps_the_human_there() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.open(&second.root).expect("second project");
        let id = comment_on(&mut workspace, &second.root, "look at this");
        workspace.active_mut().open_file_picker();

        let focus = McpRequestKind::Focus {
            target: FocusTarget::Id(id),
            note: None,
        };
        assert!(matches!(
            call(&mut workspace, None, focus),
            McpResponse::Error(_)
        ));
        assert_eq!(root(&workspace), canon(&first.root));
    }

    #[tokio::test]
    async fn feedback_in_any_project_moves_the_agents_epoch() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.open(&second.root).expect("second project");
        let before = *workspace.feedback_rx().borrow();
        workspace.tabs[1]
            .app
            .feedback_tx
            .send_modify(|epoch| *epoch += 1);
        workspace.handle(WsEvent::Input(AppEvent::Tick));
        assert_eq!(*workspace.feedback_rx().borrow(), before + 1);
    }

    #[tokio::test]
    async fn the_agent_opens_a_project_behind_the_humans_tab() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        let kind = McpRequestKind::OpenProject {
            path: second.root.display().to_string(),
        };
        let McpResponse::ProjectOpened(info) = call(&mut workspace, None, kind) else {
            panic!("opened");
        };
        assert!(!info.active);
        assert_eq!(workspace.tabs.len(), 2);
        assert_eq!(root(&workspace), canon(&first.root));
        let McpResponse::Status(status) = call(&mut workspace, None, McpRequestKind::ReviewStatus)
        else {
            panic!("status");
        };
        assert_eq!(status.projects.len(), 2);
    }

    #[tokio::test]
    async fn the_picker_opens_a_typed_path_as_a_tab_and_shows_it() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.handle(alt('n'));
        assert!(matches!(
            workspace.active().modal,
            Some(crate::app::Modal::AddProject { .. })
        ));
        for c in second.root.display().to_string().chars() {
            workspace.handle(WsEvent::Input(AppEvent::Key(KeyEvent::from(
                KeyCode::Char(c),
            ))));
        }
        workspace.handle(WsEvent::Input(AppEvent::Key(KeyEvent::from(
            KeyCode::Enter,
        ))));
        assert_eq!(workspace.tabs.len(), 2);
        assert_eq!(
            root(&workspace),
            canon(&second.root),
            "the new tab is in front"
        );
    }

    #[tokio::test]
    async fn closing_a_tab_keeps_one_project_open() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.handle(alt('w'));
        assert_eq!(workspace.tabs.len(), 1, "the only project stays");
        workspace.open(&second.root).expect("second project");
        workspace.handle(alt('l'));
        workspace.handle(alt('w'));
        assert_eq!(workspace.tabs.len(), 1);
        assert_eq!(root(&workspace), canon(&first.root));
    }

    #[tokio::test]
    async fn a_click_on_a_tab_label_switches_to_it() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.open(&second.root).expect("second project");
        workspace.sync_strip();
        let label = TabStrip::label(0, &workspace.active().project_name()).len();
        let click = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: u16::try_from(label + 2).expect("fits"),
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        workspace.handle(WsEvent::Input(AppEvent::Mouse(click)));
        assert_eq!(root(&workspace), canon(&second.root));
    }

    #[tokio::test]
    async fn the_input_pump_ending_quits() {
        let first = changed_repo();
        let (mut workspace, _rx) = workspace(&first);
        assert_eq!(workspace.handle(WsEvent::Input(AppEvent::Quit)), Flow::Quit);
    }

    #[tokio::test]
    async fn closing_waits_for_an_open_dialog() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.open(&second.root).expect("second project");
        workspace.handle(alt('n'));
        workspace.handle(alt('w'));
        assert_eq!(workspace.tabs.len(), 2, "the picker is still open");
    }

    #[tokio::test]
    async fn a_project_name_two_tabs_share_asks_for_the_path() {
        let (first, second) = (changed_repo(), changed_repo());
        let (mut workspace, _rx) = workspace(&first);
        workspace.open(&second.root).expect("second project");
        let (reply, mut rx) = oneshot::channel();
        workspace.handle(WsEvent::Mcp(AppEvent::Mcp(McpRequest {
            kind: McpRequestKind::GetDiff { file: None },
            project: Some("fixture".to_owned()),
            reply,
        })));
        let McpResponse::Error(message) = rx.try_recv().expect("an answer") else {
            panic!("refused");
        };
        assert!(message.contains("pass its path"), "{message}");
    }

    #[tokio::test]
    async fn an_unknown_project_names_the_tool_that_opens_it() {
        let first = changed_repo();
        let (mut workspace, _rx) = workspace(&first);
        let kind = McpRequestKind::GetDiff { file: None };
        let McpResponse::Error(message) =
            call(&mut workspace, Some(Path::new("/nowhere/at-all")), kind)
        else {
            panic!("refused");
        };
        assert!(message.contains("open_project"), "{message}");
    }
}
