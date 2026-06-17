//! Spike: a self-contained node-graph screen behind the `graph` subcommand.
//! Throwaway exploration to validate a navigable orthogonal graph UI and to pick
//! a layout engine — not wired into the review loop. See the spec at
//! docs/superpowers/specs/2026-06-17-node-graph-tui-spike-design.md.

mod engine;
mod github;
mod model;
#[cfg(test)]
mod tui_nodes;

use std::path::Path;

use color_eyre::eyre::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as LayoutArea, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::event::{self, AppEvent};
use crate::theme::Theme;
use engine::{GraphEngine, Layered, Layout, Zoom};
use model::{Model, NodeId, NodeStatus};

pub use model::Model as GraphModel;

/// A workflow run that can be re-polled for live status. The YAML is kept so a
/// status refresh rebuilds the model without re-reading the file.
pub struct LiveSource {
    yaml: String,
    run_id: String,
}

/// Build a model from a GitHub Actions workflow: the DAG from its `needs`, live
/// status (best-effort) from the latest run or `run`. Reading the workflow is
/// required; the `gh` calls are tolerated so the structure shows even offline.
/// Returns a [`LiveSource`] when a run id is known, so the screen can watch it.
pub fn github_source(workflow: &Path, run: Option<String>) -> Result<(Model, Option<LiveSource>)> {
    let yaml = std::fs::read_to_string(workflow)
        .wrap_err_with(|| format!("read workflow {}", workflow.display()))?;
    let run_id = run.or_else(|| github::latest_run(workflow).ok());
    let jobs = run_id
        .as_deref()
        .map(github::fetch_jobs)
        .and_then(Result::ok)
        .unwrap_or_default();
    let model = github::build_model(&yaml, &jobs)?;
    let live = run_id.map(|run_id| LiveSource {
        yaml: yaml.clone(),
        run_id,
    });
    Ok((model, live))
}

/// How often to re-poll a watched run for live status. CI state changes slowly,
/// so a relaxed interval keeps `gh` load and rate-limit pressure low.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Two left-presses within this window (at ~the same cell) are a double-click.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

/// Run the graph screen to completion. Owns terminal setup/teardown so the
/// spike stays isolated from the main review loop.
pub async fn run(model: Model, theme: Theme, live: Option<LiveSource>) -> color_eyre::Result<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    let terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let result = run_loop(terminal, model, theme, live).await;
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

async fn run_loop(
    mut terminal: ratatui::DefaultTerminal,
    model: Model,
    theme: Theme,
    live: Option<LiveSource>,
) -> color_eyre::Result<()> {
    let mut app = GraphApp::new(model, Box::new(Layered), theme);
    app.watching = live.is_some();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _events = event::spawn_event_loop(tx);

    // a watched run is polled on its own task so the gh subprocess never blocks
    // the event loop; refreshed statuses arrive on this channel
    let (status_tx, mut status_rx) = mpsc::unbounded_channel();
    let yaml = live.as_ref().map(|l| l.yaml.clone());
    if let Some(live) = live {
        tokio::spawn(poll_run(live.run_id, status_tx));
    }

    loop {
        terminal.draw(|frame| app.draw(frame))?;
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    AppEvent::Quit => break,
                    AppEvent::Key(key)
                        if key.kind != KeyEventKind::Release && app.handle_key(&key) =>
                    {
                        break;
                    }
                    AppEvent::Mouse(mouse) => app.handle_mouse(mouse),
                    _ => {}
                }
            }
            jobs = status_rx.recv() => {
                match jobs {
                    Some(jobs) => {
                        if let Some(yaml) = yaml.as_deref() {
                            app.refresh_status(yaml, &jobs);
                        }
                    }
                    None => break,
                }
            }
        }
    }
    Ok(())
}

/// Poll a run's job statuses forever, emitting each fetch on `tx`. The fetch is
/// blocking `gh`, so it runs on a blocking thread; failures are skipped (the
/// last good statuses stay on screen).
async fn poll_run(run_id: String, tx: mpsc::UnboundedSender<Vec<github::JobStatus>>) {
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    loop {
        ticker.tick().await;
        let id = run_id.clone();
        let fetched = tokio::task::spawn_blocking(move || github::fetch_jobs(&id)).await;
        if let Ok(Ok(jobs)) = fetched
            && tx.send(jobs).is_err()
        {
            break;
        }
    }
}

struct GraphApp {
    model: Model,
    engine: Box<dyn GraphEngine>,
    layout: Layout,
    selected: Option<NodeId>,
    scroll_x: u16,
    scroll_y: u16,
    viewport: Rect,
    theme: Theme,
    watching: bool,
    zoom: Zoom,
    /// Group keys currently collapsed into a single node (matrix folding).
    collapsed: std::collections::HashSet<String>,
    /// Time + cell of the last left-press, for double-click detection.
    last_click: Option<(std::time::Instant, u16, u16)>,
}

impl GraphApp {
    fn new(model: Model, engine: Box<dyn GraphEngine>, theme: Theme) -> Self {
        let mut app = Self {
            model,
            engine,
            layout: Layout::default(),
            selected: None,
            scroll_x: 0,
            scroll_y: 0,
            viewport: Rect::default(),
            theme,
            watching: false,
            zoom: Zoom::Normal,
            collapsed: std::collections::HashSet::new(),
            last_click: None,
        };
        // build the first layout through the collapse path so fold markers show
        app.relayout();
        app
    }

    /// Recompute the layout (after a zoom, collapse, or status change), keeping
    /// the selection if its node still exists.
    fn relayout(&mut self) {
        let view = self.model.collapse(&self.collapsed);
        self.layout = self.engine.lay_out(&view, self.zoom);
        let gone = self.selected.as_ref().is_none_or(|id| {
            !self
                .layout
                .placements
                .iter()
                .any(|p| &p.id == id && p.selectable)
        });
        if gone {
            self.selected = self.first_selectable();
        }
    }

    /// Toggle the selected node's foldable group. Only a group *root* is
    /// foldable; the shortcut is a no-op on members and ordinary nodes (whose
    /// Enter is reserved for a future open action). The root stays selected
    /// across the toggle since it exists in both states.
    fn toggle_collapse(&mut self) {
        let Some(group) = self
            .selected
            .as_ref()
            .and_then(|id| self.model.foldable_of(id))
        else {
            return;
        };
        if !self.collapsed.remove(&group) {
            self.collapsed.insert(group);
        }
        self.relayout();
    }

    /// Re-poll outcome: rebuild the model with fresh statuses and re-lay out.
    /// Topology is unchanged during a run, so node positions stay put — only the
    /// status glyphs/colors move. The selection survives by id.
    fn refresh_status(&mut self, yaml: &str, jobs: &[github::JobStatus]) {
        if let Ok(model) = github::build_model(yaml, jobs) {
            self.model = model;
            self.relayout();
        }
    }

    /// Returns true when the screen should exit.
    fn handle_key(&mut self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('h') | KeyCode::Left => self.step(Dir::Left),
            KeyCode::Char('l') | KeyCode::Right => self.step(Dir::Right),
            KeyCode::Char('k') | KeyCode::Up => self.step(Dir::Up),
            KeyCode::Char('j') | KeyCode::Down => self.step(Dir::Down),
            KeyCode::Char('n') => self.follow(true),
            KeyCode::Char('N') => self.follow(false),
            KeyCode::Char('g') => self.select_end(true),
            KeyCode::Char('G') => self.select_end(false),
            KeyCode::Char('+' | '=') => {
                self.zoom = self.zoom.in_();
                self.relayout();
            }
            KeyCode::Char('-' | '_') => {
                self.zoom = self.zoom.out();
                self.relayout();
            }
            KeyCode::Enter | KeyCode::Char('c') => self.toggle_collapse(),
            _ => {}
        }
        self.ensure_visible();
        false
    }

    fn selected_placement(&self) -> Option<&engine::Placement> {
        let id = self.selected.as_ref()?;
        self.layout.placements.iter().find(|p| &p.id == id)
    }

    /// Move the selection to the nearest node in `dir` from the current one.
    fn step(&mut self, dir: Dir) {
        let Some(from) = self.selected_placement().map(center) else {
            return;
        };
        let best = self
            .layout
            .placements
            .iter()
            .filter_map(|p| {
                // horizontal moves cross columns and land on gates only;
                // vertical moves can descend into a group's legs
                let reachable = match dir {
                    Dir::Left | Dir::Right => p.selectable,
                    Dir::Up | Dir::Down => p.selectable || p.member,
                };
                if !reachable {
                    return None;
                }
                let to = center(p);
                let (dx, dy) = (
                    i32::from(to.0) - i32::from(from.0),
                    i32::from(to.1) - i32::from(from.1),
                );
                let ahead = match dir {
                    Dir::Left => dx < 0,
                    Dir::Right => dx > 0,
                    Dir::Up => dy < 0,
                    Dir::Down => dy > 0,
                };
                if !ahead {
                    return None;
                }
                // primary axis distance dominates; secondary axis breaks ties
                let cost = match dir {
                    Dir::Left | Dir::Right => dx.abs() * 3 + dy.abs(),
                    Dir::Up | Dir::Down => dy.abs() * 3 + dx.abs(),
                };
                Some((cost, p.id.clone()))
            })
            .min_by_key(|(cost, _)| *cost);
        if let Some((_, id)) = best {
            self.selected = Some(id);
        }
    }

    /// Follow an outgoing (`forward`) or incoming edge from the selection.
    fn follow(&mut self, forward: bool) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let next = self.model.edges.iter().find_map(|e| {
            if forward && e.from == id {
                Some(e.to.clone())
            } else if !forward && e.to == id {
                Some(e.from.clone())
            } else {
                None
            }
        });
        if let Some(next) = next {
            self.selected = Some(next);
        }
    }

    /// Mouse: a left-click selects the node under the pointer; a double-click
    /// toggles its group (like Enter). Scroll pans vertically.
    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.is_double_click(mouse.column, mouse.row);
                if let Some(id) = self.node_at(mouse.column, mouse.row) {
                    self.selected = Some(id);
                    if double {
                        self.toggle_collapse();
                    }
                    self.ensure_visible();
                }
            }
            MouseEventKind::ScrollDown => self.scroll_y = self.scroll_y.saturating_add(2),
            MouseEventKind::ScrollUp => self.scroll_y = self.scroll_y.saturating_sub(2),
            _ => {}
        }
    }

    /// The node whose box covers screen cell `(col, row)`. A leg inside a
    /// container wins over the container, so clicking a leg selects the leg.
    fn node_at(&self, col: u16, row: u16) -> Option<NodeId> {
        let v = self.viewport;
        if col < v.x || col >= v.x + v.width || row < v.y || row >= v.y + v.height {
            return None;
        }
        let gx = self.scroll_x + (col - v.x);
        let gy = self.scroll_y + (row - v.y);
        let covers =
            |p: &&engine::Placement| gx >= p.x && gx < p.x + p.w && gy >= p.y && gy < p.y + p.h;
        self.layout
            .placements
            .iter()
            .find(|p| p.member && covers(p))
            .or_else(|| {
                self.layout
                    .placements
                    .iter()
                    .find(|p| p.selectable && covers(p))
            })
            .map(|p| p.id.clone())
    }

    /// A second left-press at (about) the same cell within the double-click
    /// window. Resets after firing so a third press starts fresh.
    fn is_double_click(&mut self, col: u16, row: u16) -> bool {
        let now = std::time::Instant::now();
        let double = self.last_click.is_some_and(|(at, c, r)| {
            now.duration_since(at) < DOUBLE_CLICK_WINDOW && c.abs_diff(col) <= 1 && r == row
        });
        self.last_click = if double { None } else { Some((now, col, row)) };
        double
    }

    /// Jump to the first (`top`) or last navigable node in layout order.
    fn select_end(&mut self, top: bool) {
        let mut navigable = self.layout.placements.iter().filter(|p| p.selectable);
        let pick = if top {
            navigable.next()
        } else {
            navigable.next_back()
        };
        if let Some(p) = pick {
            self.selected = Some(p.id.clone());
        }
    }

    /// The first navigable node, for seeding/resetting the selection.
    fn first_selectable(&self) -> Option<NodeId> {
        self.layout
            .placements
            .iter()
            .find(|p| p.selectable)
            .map(|p| p.id.clone())
    }

    /// Scroll so the selected node stays inside the viewport.
    fn ensure_visible(&mut self) {
        let (vw, vh) = (self.viewport.width, self.viewport.height);
        if vw == 0 || vh == 0 {
            return; // no render yet — viewport unknown
        }
        let Some((x, y, w, h)) = self.selected_placement().map(|p| (p.x, p.y, p.w, p.h)) else {
            return;
        };
        if x < self.scroll_x {
            self.scroll_x = x;
        } else if x + w >= self.scroll_x + vw {
            self.scroll_x = (x + w).saturating_sub(vw).saturating_add(1);
        }
        if y < self.scroll_y {
            self.scroll_y = y;
        } else if y + h >= self.scroll_y + vh {
            self.scroll_y = (y + h).saturating_sub(vh).saturating_add(1);
        }
        // never scroll past the laid-out content
        self.scroll_x = self.scroll_x.min(self.layout.width.saturating_sub(vw));
        self.scroll_y = self.scroll_y.min(self.layout.height.saturating_sub(vh));
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        frame.render_widget(
            ratatui::widgets::Block::new().style(self.theme.base()),
            area,
        );
        let [hint, body, bar] = LayoutArea::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        self.viewport = body;
        // keep the selection in view; wheel panning sticks until the selection
        // would scroll off (then this clamps it back)
        self.ensure_visible();

        frame.render_widget(Paragraph::new(self.hint_line()), hint);

        // base layer: the engine's art grid, scrolled to the viewport
        let lines: Vec<Line<'static>> = self
            .layout
            .lines
            .iter()
            .map(|l| Line::styled(l.clone(), Style::new().fg(self.theme.dim)))
            .collect();
        frame.render_widget(
            Paragraph::new(lines).scroll((self.scroll_y, self.scroll_x)),
            body,
        );

        self.paint_nodes(frame, body);
        frame.render_widget(
            Paragraph::new(self.status_line()).style(Style::new().bg(self.theme.panel)),
            bar,
        );
    }

    /// Recolor each visible node's cells by status, bolding the selection. This
    /// is the "status highlight" overlay on top of the engine's art.
    fn paint_nodes(&self, frame: &mut Frame<'_>, body: Rect) {
        let buf = frame.buffer_mut();
        for p in &self.layout.placements {
            let selected = self.selected.as_ref() == Some(&p.id);
            let color = status_color(&self.theme, p.status);
            for row in 0..p.h {
                let gy = p.y + row;
                if gy < self.scroll_y {
                    continue;
                }
                let sy = body.y + (gy - self.scroll_y);
                if sy >= body.y + body.height {
                    continue;
                }
                // a container colors only its outline, so the member boxes
                // inside keep their own status colors
                let border_row = row == 0 || row + 1 == p.h;
                for col in 0..p.w {
                    if p.container && !border_row && col != 0 && col + 1 != p.w {
                        continue;
                    }
                    let gx = p.x + col;
                    if gx < self.scroll_x {
                        continue;
                    }
                    let sx = body.x + (gx - self.scroll_x);
                    if sx >= body.x + body.width {
                        continue;
                    }
                    if let Some(cell) = buf.cell_mut((sx, sy)) {
                        cell.set_fg(color);
                        if selected {
                            cell.modifier.insert(Modifier::BOLD | Modifier::REVERSED);
                        }
                    }
                }
            }
        }
    }

    fn hint_line(&self) -> Line<'static> {
        Line::styled(
            " hjkl move · n/N edge · ⏎ collapse · +/- zoom · g/G ends · q quit".to_owned(),
            Style::new().fg(self.theme.dim),
        )
    }

    fn status_line(&self) -> Line<'static> {
        let sel = self
            .selected
            .as_ref()
            .map_or("-", |id| id.0.as_str())
            .to_owned();
        let watch = if self.watching { "  ⟳ watching" } else { "" };
        Line::styled(
            format!(
                " GRAPH  engine: {}  zoom: {}  nodes: {}  sel: {sel}{watch}",
                self.engine.name(),
                self.zoom.label(),
                self.model.nodes.len(),
            ),
            Style::new().fg(self.theme.fg).bg(self.theme.panel),
        )
    }
}

#[derive(Clone, Copy)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The point navigation measures from. A container uses its top so that `j`
/// from it enters the first leg and `j`/`k` then cycle the legs cleanly,
/// instead of jumping into the middle of the stack.
fn center(p: &engine::Placement) -> (u16, u16) {
    let cy = if p.container { p.y } else { p.y + p.h / 2 };
    (p.x + p.w / 2, cy)
}

fn status_color(theme: &Theme, status: NodeStatus) -> ratatui::style::Color {
    match status {
        NodeStatus::Ok => theme.added,
        NodeStatus::Failed => theme.error_fg,
        NodeStatus::Running => theme.warn_fg,
        NodeStatus::Queued | NodeStatus::Skipped => theme.dim,
        NodeStatus::Neutral => theme.fg,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn app() -> GraphApp {
        GraphApp::new(Model::demo(), Box::new(Layered), Theme::github_dark())
    }

    fn render(app: &mut GraphApp) -> Terminal<TestBackend> {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        terminal
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), crossterm::event::KeyModifiers::NONE)
    }

    fn left_click(col: u16, row: u16) -> crossterm::event::MouseEvent {
        use crossterm::event::{MouseButton, MouseEventKind};
        crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    /// A screen cell on a placement: a container's title row (so the click hits
    /// the container, not a leg inside it), else the box centre.
    fn cell_of(app: &GraphApp, id: &str) -> (u16, u16) {
        let p = app
            .layout
            .placements
            .iter()
            .find(|p| p.id == NodeId::new(id))
            .expect("placement");
        let (gx, gy) = if p.container {
            (p.x + 3, p.y)
        } else {
            (p.x + p.w / 2, p.y + p.h / 2)
        };
        (
            app.viewport.x + gx.saturating_sub(app.scroll_x),
            app.viewport.y + gy.saturating_sub(app.scroll_y),
        )
    }

    #[test]
    fn mouse_click_selects_and_double_click_folds() {
        let mut app = app();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");

        // a single click on the container selects it (not a leg inside)
        let (cx, cy) = cell_of(&app, "test");
        app.handle_mouse(left_click(cx, cy));
        assert_eq!(app.selected, Some(NodeId::new("test")));
        // clicking a leg directly selects the leg, not the container
        let (lx, ly) = cell_of(&app, "test ubuntu");
        app.handle_mouse(left_click(lx, ly));
        assert_eq!(
            app.selected,
            Some(NodeId::new("test ubuntu")),
            "click reaches the leg"
        );

        // double-click folds the group; do it on the container
        app.handle_mouse(left_click(cx, cy));
        app.handle_mouse(left_click(cx, cy));
        assert!(app.collapsed.contains("test"), "double-click folds");
    }

    #[test]
    fn vertical_nav_enters_group_legs_horizontal_stays_on_the_gate() {
        let mut app = app();
        render(&mut app); // set the viewport so ensure_visible behaves
        // from the left column, right lands on the container gate, not a leg
        app.selected = Some(NodeId::new("lint"));
        app.handle_key(&key('l'));
        assert_eq!(
            app.selected,
            Some(NodeId::new("test")),
            "l enters at the gate"
        );
        // down descends into the legs
        app.handle_key(&key('j'));
        let sel = app.selected.clone().expect("selection");
        let node = app
            .model
            .nodes
            .iter()
            .find(|n| n.id == sel)
            .expect("selected node");
        assert_eq!(
            node.group.as_deref(),
            Some("test"),
            "j enters a leg of the group"
        );
    }

    #[test]
    fn demo_graph_renders() {
        let mut app = app();
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn only_the_group_root_folds_and_round_trips() {
        let mut app = app();
        // a matrix leg is not foldable: the shortcut is a no-op on it
        app.selected = Some(NodeId::new("test ubuntu"));
        app.handle_key(&key('c'));
        assert!(app.collapsed.is_empty(), "a leg does not accept collapse");

        // the root folds; selection stays on it across the toggle
        app.selected = Some(NodeId::new("test"));
        app.handle_key(&key('c'));
        assert!(app.collapsed.contains("test"));
        assert_eq!(app.selected, Some(NodeId::new("test")));
        let folded = app
            .model
            .collapse(&app.collapsed)
            .nodes
            .iter()
            .find(|n| n.id == NodeId::new("test"))
            .cloned()
            .expect("root node present");
        assert_eq!(folded.label, "▸ test (3)", "marker + leg count");
        assert_eq!(folded.status, NodeStatus::Failed, "failed leg dominates");
        insta::assert_snapshot!(render(&mut app).backend());

        // Enter expands it back; the root stays selected
        let enter = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        app.handle_key(&enter);
        assert!(app.collapsed.is_empty(), "expanded back");
        assert_eq!(app.selected, Some(NodeId::new("test")));
    }

    #[test]
    fn zoom_out_renders_a_compact_overview() {
        let mut app = app();
        app.handle_key(&key('-'));
        assert_eq!(app.zoom, Zoom::Compact);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn zoom_in_renders_detail_with_status() {
        let mut app = app();
        app.handle_key(&key('+'));
        assert_eq!(app.zoom, Zoom::Detail);
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn code_graph_renders() {
        // a cyclic call graph (eval/apply recursion) — exercises back-edges
        let mut app = GraphApp::new(Model::code_demo(), Box::new(Layered), Theme::github_dark());
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn release_workflow_graph_renders() {
        // the real release pipeline, parsed from the checked-in workflow
        let yaml = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.github/workflows/release.yml"
        ));
        let model = github::build_model(yaml, &[]).expect("release.yml parses");
        let mut app = GraphApp::new(model, Box::new(Layered), Theme::github_dark());
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn navigation_moves_the_selection() {
        let mut app = app();
        let first = app.selected.clone();
        // follow an outgoing edge, then a downward step — selection must change
        app.handle_key(&key('n'));
        assert_ne!(app.selected, first, "n follows an edge to another node");
        let after_n = app.selected.clone();
        app.handle_key(&key('g'));
        assert_eq!(app.selected, first, "g returns to the first node");
        assert_ne!(app.selected, after_n);
    }
}
