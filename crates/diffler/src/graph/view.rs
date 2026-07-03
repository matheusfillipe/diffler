//! `GraphView`: the reusable, IO-free graph component. The host pushes a
//! [`Model`] in, renders it into any area, and reacts to the [`GraphAction`]s
//! it emits. It owns only view state (selection, scroll, zoom, collapsed
//! groups) — no terminal, no event loop, no sources.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::graph::engine::{GraphEngine, Layered, Layout, Placement, Zoom};
use crate::graph::model::{Model, NodeId, NodeStatus};
use crate::graph::theme::GraphTheme;

/// What the component asks the host to do. The host owns the side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphAction {
    /// Enter / double-click on a non-foldable node — open it (code, log, …).
    Activated(NodeId),
    /// A group was folded or unfolded.
    Folded { group: String, collapsed: bool },
}

/// Two left-presses within this window (at ~the same cell) are a double-click.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

pub struct GraphView {
    model: Model,
    // `+ Send` so an embedding host that crosses threads/await points (the
    // review app is spawned in tests) keeps `App: Send`
    engine: Box<dyn GraphEngine + Send>,
    layout: Layout,
    selected: Option<NodeId>,
    scroll_x: u16,
    scroll_y: u16,
    viewport: Rect,
    zoom: Zoom,
    collapsed: HashSet<String>,
    marks: HashSet<NodeId>,
    last_click: Option<(std::time::Instant, u16, u16)>,
}

impl Default for GraphView {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphView {
    #[must_use]
    pub fn new() -> Self {
        Self {
            model: Model::new(crate::graph::model::RankDir::LeftRight),
            engine: Box::new(Layered),
            layout: Layout::default(),
            selected: None,
            scroll_x: 0,
            scroll_y: 0,
            viewport: Rect::default(),
            zoom: Zoom::Normal,
            collapsed: HashSet::new(),
            marks: HashSet::new(),
            last_click: None,
        }
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    pub fn selected(&self) -> Option<&NodeId> {
        self.selected.as_ref()
    }

    // --- state in: the host signals state dynamically ---

    /// Replace the whole graph (topology changed). Keeps the selection if it
    /// survives.
    pub fn set_model(&mut self, model: Model) {
        self.model = model;
        self.relayout();
    }

    /// Update node statuses in place (e.g. a live CI poll) without changing
    /// topology, then re-lay out — positions stay put, colors/glyphs move.
    pub fn patch_status(&mut self, updates: impl IntoIterator<Item = (NodeId, NodeStatus)>) {
        for (id, status) in updates {
            if let Some(node) = self.model.nodes.iter_mut().find(|n| n.id == id) {
                node.status = status;
            }
        }
        self.relayout();
    }

    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom;
        self.relayout();
    }

    pub fn set_collapsed(&mut self, group: &str, collapsed: bool) {
        let changed = if collapsed {
            self.collapsed.insert(group.to_owned())
        } else {
            self.collapsed.remove(group)
        };
        if changed {
            self.relayout();
        }
    }

    pub fn select(&mut self, id: &NodeId) {
        if self.layout.placements.iter().any(|p| &p.id == id) {
            self.selected = Some(id.clone());
            self.ensure_visible();
        }
    }

    /// The searchable nodes in placement order: `(row, label)` pairs feeding
    /// the shared `/` search, one row per visible node.
    pub fn search_rows(&self) -> Vec<(usize, String)> {
        self.searchable()
            .enumerate()
            .map(|(row, id)| {
                let label = self
                    .model
                    .nodes
                    .iter()
                    .find(|n| &n.id == id)
                    .map_or_else(|| id.0.clone(), |n| n.label.clone());
                (row, label)
            })
            .collect()
    }

    /// Select the node at `row` of [`Self::search_rows`], scrolling it into view.
    pub fn select_nth(&mut self, row: usize) {
        let id = self.searchable().nth(row).cloned();
        if let Some(id) = id {
            self.selected = Some(id);
            self.ensure_visible();
        }
    }

    /// The selection's row in [`Self::search_rows`]; `0` when nothing is selected.
    pub fn selected_index(&self) -> usize {
        let Some(selected) = self.selected.as_ref() else {
            return 0;
        };
        self.searchable().position(|id| id == selected).unwrap_or(0)
    }

    /// Mark the given [`Self::search_rows`] rows as search matches; they render
    /// on the search background until replaced.
    pub fn set_marks(&mut self, rows: &[usize]) {
        let ids: Vec<NodeId> = self.searchable().cloned().collect();
        self.marks = rows
            .iter()
            .filter_map(|row| ids.get(*row).cloned())
            .collect();
    }

    fn searchable(&self) -> impl Iterator<Item = &NodeId> {
        self.layout
            .placements
            .iter()
            .filter(|p| p.selectable || p.member)
            .map(|p| &p.id)
    }

    // --- input: pure; returns an action for the host ---

    /// Handle a key. Returns an action the host should act on, or `None`.
    /// Navigation/zoom/collapse are handled internally; Enter on a non-foldable
    /// node yields `Activated`. Quit/back keys are the host's concern.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<GraphAction> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let action = match key.code {
            KeyCode::Char('h') | KeyCode::Left => self.step_none(Dir::Left),
            KeyCode::Char('l') | KeyCode::Right => self.step_none(Dir::Right),
            KeyCode::Char('k') | KeyCode::Up => self.step_none(Dir::Up),
            KeyCode::Char('j') | KeyCode::Down => self.step_none(Dir::Down),
            KeyCode::Char('n') => self.follow_none(true),
            KeyCode::Char('N') => self.follow_none(false),
            KeyCode::Char('g') => self.end_none(true),
            KeyCode::Char('G') => self.end_none(false),
            KeyCode::Char('+' | '=') => {
                self.set_zoom(self.zoom.in_());
                None
            }
            KeyCode::Char('-' | '_') => {
                self.set_zoom(self.zoom.out());
                None
            }
            KeyCode::Enter | KeyCode::Char('c') => return self.activate(),
            _ => None,
        };
        self.ensure_visible();
        action
    }

    /// Handle a mouse event: click selects (a leg wins over its container),
    /// double-click activates (fold or open), wheel pans.
    pub fn on_mouse(&mut self, mouse: MouseEvent) -> Option<GraphAction> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.is_double_click(mouse.column, mouse.row);
                let id = self.node_at(mouse.column, mouse.row)?;
                self.selected = Some(id);
                self.ensure_visible();
                if double {
                    return self.activate();
                }
                None
            }
            MouseEventKind::ScrollDown => {
                self.scroll_y = self.scroll_y.saturating_add(2);
                None
            }
            MouseEventKind::ScrollUp => {
                self.scroll_y = self.scroll_y.saturating_sub(2);
                None
            }
            _ => None,
        }
    }

    // --- render ---

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &GraphTheme) {
        self.viewport = area;
        self.ensure_visible();
        let lines: Vec<Line<'static>> = self
            .layout
            .lines
            .iter()
            .map(|l| Line::styled(l.clone(), Style::new().fg(theme.dim).bg(theme.bg)))
            .collect();
        Paragraph::new(lines)
            .scroll((self.scroll_y, self.scroll_x))
            .render(area, buf);
        self.paint_nodes(area, buf, theme);
    }

    // --- internals ---

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

    /// Enter on the selection: fold a group root, else open a plain node.
    fn activate(&mut self) -> Option<GraphAction> {
        let id = self.selected.clone()?;
        if let Some(group) = self.model.foldable_of(&id) {
            let collapsed = !self.collapsed.remove(&group);
            if collapsed {
                self.collapsed.insert(group.clone());
            }
            self.relayout();
            self.ensure_visible();
            Some(GraphAction::Folded { group, collapsed })
        } else {
            Some(GraphAction::Activated(id))
        }
    }

    fn step_none(&mut self, dir: Dir) -> Option<GraphAction> {
        self.step(dir);
        None
    }

    fn follow_none(&mut self, forward: bool) -> Option<GraphAction> {
        self.follow(forward);
        None
    }

    fn end_none(&mut self, top: bool) -> Option<GraphAction> {
        self.select_end(top);
        None
    }

    fn selected_placement(&self) -> Option<&Placement> {
        let id = self.selected.as_ref()?;
        self.layout.placements.iter().find(|p| &p.id == id)
    }

    fn step(&mut self, dir: Dir) {
        let Some((from, from_x)) = self.selected_placement().map(|p| (center(p), p.x)) else {
            return;
        };
        let best = self
            .layout
            .placements
            .iter()
            .filter_map(|p| {
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
                // horizontal moves measure column edges, not centers: nodes in
                // one column share their left x but not their width, so a wide
                // same-column cousin's center would read as nearer/"ahead"
                let edge_dx = i32::from(p.x) - i32::from(from_x);
                let ahead = match dir {
                    Dir::Left => edge_dx < 0,
                    Dir::Right => edge_dx > 0,
                    Dir::Up => dy < 0,
                    Dir::Down => dy > 0,
                };
                if !ahead {
                    return None;
                }
                let cost = match dir {
                    Dir::Left | Dir::Right => edge_dx.abs() * 3 + dy.abs(),
                    Dir::Up | Dir::Down => dy.abs() * 3 + dx.abs(),
                };
                Some((cost, p.id.clone()))
            })
            .min_by_key(|(cost, _)| *cost);
        if let Some((_, id)) = best {
            self.selected = Some(id);
        }
    }

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

    fn first_selectable(&self) -> Option<NodeId> {
        self.layout
            .placements
            .iter()
            .find(|p| p.selectable)
            .map(|p| p.id.clone())
    }

    fn ensure_visible(&mut self) {
        let (vw, vh) = (self.viewport.width, self.viewport.height);
        if vw == 0 || vh == 0 {
            return;
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
        self.scroll_x = self.scroll_x.min(self.layout.width.saturating_sub(vw));
        self.scroll_y = self.scroll_y.min(self.layout.height.saturating_sub(vh));
    }

    fn node_at(&self, col: u16, row: u16) -> Option<NodeId> {
        let v = self.viewport;
        if col < v.x || col >= v.x + v.width || row < v.y || row >= v.y + v.height {
            return None;
        }
        let gx = self.scroll_x + (col - v.x);
        let gy = self.scroll_y + (row - v.y);
        let covers = |p: &&Placement| gx >= p.x && gx < p.x + p.w && gy >= p.y && gy < p.y + p.h;
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

    fn is_double_click(&mut self, col: u16, row: u16) -> bool {
        let now = std::time::Instant::now();
        let double = self.last_click.is_some_and(|(at, c, r)| {
            now.duration_since(at) < DOUBLE_CLICK_WINDOW && c.abs_diff(col) <= 1 && r == row
        });
        self.last_click = if double { None } else { Some((now, col, row)) };
        double
    }

    /// Recolor each node's cells by status (containers: border only, so member
    /// boxes keep their own colors); the selection is bold + reversed.
    fn paint_nodes(&self, area: Rect, buf: &mut Buffer, theme: &GraphTheme) {
        for p in &self.layout.placements {
            let selected = self.selected.as_ref() == Some(&p.id);
            let marked = self.marks.contains(&p.id);
            let color = theme.status(p.status);
            for row in 0..p.h {
                let gy = p.y + row;
                if gy < self.scroll_y {
                    continue;
                }
                let sy = area.y + (gy - self.scroll_y);
                if sy >= area.y + area.height {
                    continue;
                }
                let border_row = row == 0 || row + 1 == p.h;
                for col in 0..p.w {
                    if p.container && !border_row && col != 0 && col + 1 != p.w {
                        continue;
                    }
                    let gx = p.x + col;
                    if gx < self.scroll_x {
                        continue;
                    }
                    let sx = area.x + (gx - self.scroll_x);
                    if sx >= area.x + area.width {
                        continue;
                    }
                    if let Some(cell) = buf.cell_mut((sx, sy)) {
                        cell.set_fg(color);
                        if marked {
                            cell.set_bg(theme.search);
                        }
                        if selected {
                            cell.modifier.insert(Modifier::BOLD | Modifier::REVERSED);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The point navigation measures from. A container uses its top so `j` enters
/// the first leg and `j`/`k` cycle the legs cleanly.
fn center(p: &Placement) -> (u16, u16) {
    let cy = if p.container { p.y } else { p.y + p.h / 2 };
    (p.x + p.w / 2, cy)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;

    fn theme() -> GraphTheme {
        GraphTheme {
            bg: Color::Black,
            fg: Color::White,
            dim: Color::DarkGray,
            ok: Color::Green,
            failed: Color::Red,
            running: Color::Yellow,
            queued: Color::DarkGray,
            panel: Color::Black,
            search: Color::Rgb(0x57, 0x52, 0x1c),
        }
    }

    fn view() -> GraphView {
        let mut v = GraphView::new();
        v.set_model(Model::demo());
        v
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), crossterm::event::KeyModifiers::NONE)
    }

    fn render(v: &mut GraphView) -> Terminal<TestBackend> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| v.render(frame.area(), frame.buffer_mut(), &theme()))
            .expect("draw");
        terminal
    }

    #[test]
    fn renders_the_demo_with_a_group_container() {
        let mut v = view();
        let art = render(&mut v).backend().to_string();
        assert!(art.contains("▾ test"), "group root marked foldable: {art}");
        assert!(art.contains("test ubuntu"), "legs shown when expanded");
        insta::assert_snapshot!(render(&mut v).backend());
    }

    #[test]
    fn horizontal_nav_enters_the_gate_vertical_enters_legs() {
        let mut v = view();
        render(&mut v);
        v.select(&NodeId::new("lint"));
        v.on_key(key('l'));
        assert_eq!(
            v.selected(),
            Some(&NodeId::new("test")),
            "l lands on the container gate"
        );
        v.on_key(key('j'));
        assert_eq!(
            v.selected().map(|id| id.0.as_str()),
            Some("test ubuntu"),
            "j descends into the first leg"
        );
    }

    #[test]
    fn enter_folds_a_group_and_emits_an_action() {
        let mut v = view();
        render(&mut v);
        v.select(&NodeId::new("test"));
        let action = v.on_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(
            action,
            Some(GraphAction::Folded {
                group: "test".to_owned(),
                collapsed: true,
            })
        );
        assert!(render(&mut v).backend().to_string().contains("▸ test (3)"));
    }

    #[test]
    fn enter_on_a_plain_node_activates_it() {
        let mut v = view();
        render(&mut v);
        v.select(&NodeId::new("build"));
        let action = v.on_key(KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(action, Some(GraphAction::Activated(NodeId::new("build"))));
    }

    #[test]
    fn horizontal_step_crosses_columns_never_same_column_cousins() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::LeftRight);
        let n = |id: &str| Node::leaf(id, NodeStatus::Neutral);
        model.nodes = vec![n("a"), n("a very wide same-column cousin label"), n("next")];
        let e = |from: &str, to: &str| Edge {
            from: NodeId::new(from),
            to: NodeId::new(to),
            label: None,
        };
        model.edges = vec![e("a", "next")];
        let mut v = GraphView::new();
        v.set_model(model);
        render(&mut v);

        v.select(&NodeId::new("a"));
        v.on_key(key('l'));
        assert_eq!(
            v.selected(),
            Some(&NodeId::new("next")),
            "l crosses to the next column, not the wide cousin below"
        );
        v.on_key(key('h'));
        assert_eq!(v.selected(), Some(&NodeId::new("a")));
        v.on_key(key('j'));
        assert_eq!(
            v.selected().map(|id| id.0.as_str()),
            Some("a very wide same-column cousin label"),
            "cousins stay reachable vertically"
        );
    }

    #[test]
    fn search_rows_index_into_select_nth_and_marks() {
        let mut v = view();
        render(&mut v);
        let rows = v.search_rows();
        let build = rows
            .iter()
            .find(|(_, label)| label == "build")
            .expect("build row");
        v.select_nth(build.0);
        assert_eq!(v.selected(), Some(&NodeId::new("build")));
        assert_eq!(v.selected_index(), build.0);

        v.set_marks(&[build.0]);
        let terminal = render(&mut v);
        let marked_cells = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|c| c.bg == Color::Rgb(0x57, 0x52, 0x1c))
            .count();
        assert!(marked_cells > 0, "the marked node paints the search bg");
    }

    #[test]
    fn patch_status_updates_a_node_without_changing_topology() {
        let mut v = view();
        render(&mut v);
        let before = v.layout.placements.len();
        v.patch_status([(NodeId::new("build"), NodeStatus::Failed)]);
        assert_eq!(v.layout.placements.len(), before, "topology unchanged");
        assert!(render(&mut v).backend().to_string().contains("build ✗"));
    }
}
