//! Layout + render engines behind one trait, so the renderer is swappable
//! (ego-centric/radial for LSP maps later) without touching the view.
//! The view consumes an owned [`Layout`] (no engine lifetimes leak out).
//!
//! `Layered` is the favoured engine: longest-path layering assigns each node a
//! column, then it draws the GitHub-style look — outlined rounded boxes laid out
//! left-to-right, wired by clean orthogonal rails.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::graph::model::{Model, NodeId, NodeStatus};

/// An owned node rectangle in layout-grid cells, plus what the view needs to
/// color it.
#[derive(Debug, Clone)]
pub struct Placement {
    pub id: NodeId,
    pub status: NodeStatus,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// A group container (its members are drawn inside). The view colors only
    /// its border, leaving the member boxes their own colors.
    pub container: bool,
    /// A top-level nav gate (ordinary node or container). Horizontal moves and
    /// `g`/`G` only land on these — crossing columns enters a group at its
    /// container, never a leg.
    pub selectable: bool,
    /// A group leg drawn inside a container. Reachable by vertical moves (enter
    /// the group with `j`/`k`) and by clicking it directly.
    pub member: bool,
}

/// Engine output: the rendered art grid plus node placements, all owned so the
/// view holds it across frames without borrowing the engine.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub lines: Vec<String>,
    pub width: u16,
    pub height: u16,
    pub placements: Vec<Placement>,
}

pub trait GraphEngine {
    fn name(&self) -> &'static str;
    fn lay_out(&self, model: &Model, zoom: Zoom) -> Layout;
}

/// Level-of-detail. Terminal cells can't sub-cell scale, so "zoom" trades box
/// size + label detail for how much graph fits: out = compact overview, in =
/// roomy boxes with a status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    Compact,
    Normal,
    Detail,
}

impl Zoom {
    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Normal => "normal",
            Self::Detail => "detail",
        }
    }

    #[must_use]
    pub fn out(self) -> Self {
        match self {
            Self::Detail => Self::Normal,
            _ => Self::Compact,
        }
    }

    #[must_use]
    pub fn in_(self) -> Self {
        match self {
            Self::Compact => Self::Normal,
            _ => Self::Detail,
        }
    }

    /// `(box height, row gap, column gap)`.
    fn metrics(self) -> (usize, usize, usize) {
        match self {
            Self::Compact => (1, 0, 3),
            Self::Normal => (3, 1, 6),
            Self::Detail => (4, 1, 8),
        }
    }

    /// Max label chars before eliding (compact only).
    fn label_max(self) -> Option<usize> {
        match self {
            Self::Compact => Some(12),
            _ => None,
        }
    }

    fn show_meta(self) -> bool {
        self == Self::Detail
    }
}

/// GitHub-style layered renderer: longest-path layering ranks the nodes; we draw
/// rounded outlined boxes left-to-right and route orthogonal rails between columns.
pub struct Layered;

impl GraphEngine for Layered {
    fn name(&self) -> &'static str {
        "layered"
    }

    fn lay_out(&self, model: &Model, zoom: Zoom) -> Layout {
        let ranks = rank_nodes(model);
        place_and_draw(model, &ranks, zoom)
    }
}

/// Per-node `(column, row-within-column)` from a layered pass: the column is the
/// longest path from a source; within a column, nodes keep declaration order.
/// Only flow nodes (ordinary + group roots) are ranked — group members live
/// inside their root's container, not in the column flow. Cycles (call/reference
/// graphs) are tolerated: a back-edge to a node already on the path adds no depth.
fn rank_nodes(model: &Model) -> Vec<(usize, usize)> {
    let is_flow = |index: usize| model.nodes.get(index).is_some_and(|n| n.group.is_none());

    let mut preds: HashMap<usize, Vec<usize>> = HashMap::new();
    for edge in &model.edges {
        if let (Some(from), Some(to)) = (model.index_of(&edge.from), model.index_of(&edge.to))
            && from != to
            && is_flow(from)
            && is_flow(to)
        {
            preds.entry(to).or_default().push(from);
        }
    }

    let mut level: HashMap<usize, usize> = HashMap::new();
    let mut path: HashSet<usize> = HashSet::new();
    for index in 0..model.nodes.len() {
        if is_flow(index) {
            longest_path(index, &preds, &mut level, &mut path);
        }
    }

    let mut next_row: HashMap<usize, usize> = HashMap::new();
    let mut ranks = vec![(0usize, 0usize); model.nodes.len()];
    for index in 0..model.nodes.len() {
        if !is_flow(index) {
            continue;
        }
        let column = level.get(&index).copied().unwrap_or(0);
        let row = next_row.entry(column).or_default();
        if let Some(slot) = ranks.get_mut(index) {
            *slot = (column, *row);
        }
        *row += 1;
    }
    ranks
}

/// Longest path from a source to `node`, memoized into `level`. A predecessor
/// already on the current `path` is a cycle back-edge and contributes no depth.
fn longest_path(
    node: usize,
    preds: &HashMap<usize, Vec<usize>>,
    level: &mut HashMap<usize, usize>,
    path: &mut HashSet<usize>,
) -> usize {
    if let Some(&cached) = level.get(&node) {
        return cached;
    }
    path.insert(node);
    let parents = preds.get(&node).cloned().unwrap_or_default();
    let mut depth = 0;
    for parent in parents {
        if path.contains(&parent) {
            continue;
        }
        depth = depth.max(longest_path(parent, preds, level, path) + 1);
    }
    path.remove(&node);
    level.insert(node, depth);
    depth
}

// cohesive layout pass: size nodes, place columns, place container members,
// route, draw — splitting it would only scatter the shared coordinate maps
#[allow(clippy::too_many_lines)]
fn place_and_draw(model: &Model, ranks: &[(usize, usize)], zoom: Zoom) -> Layout {
    let (box_h, row_gap, col_gap) = zoom.metrics();
    let col_of = |index: usize| ranks.get(index).map_or(0, |(c, _)| *c);

    let text: Vec<String> = model
        .nodes
        .iter()
        .map(|n| label_text(&n.label, n.status, zoom))
        .collect();
    // members per group root, in model order
    let mut members: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (index, node) in model.nodes.iter().enumerate() {
        if let Some(group) = &node.group {
            members.entry(group.clone()).or_default().push(index);
        }
    }
    let members_of = |index: usize| -> &[usize] {
        model
            .nodes
            .get(index)
            .and_then(|n| n.foldable.as_ref())
            .and_then(|g| members.get(g))
            .map_or(&[][..], Vec::as_slice)
    };

    // size every node: members get a plain box; a flow node is either a box or,
    // when it has present members, a container sized to hold them stacked
    let mut size = vec![(0usize, 0usize); model.nodes.len()];
    for (index, label) in text.iter().enumerate() {
        if let Some(slot) = size.get_mut(index) {
            *slot = (label.chars().count() + 4, box_h);
        }
    }
    let flow: Vec<usize> = (0..model.nodes.len())
        .filter(|&i| model.nodes.get(i).is_some_and(|n| n.group.is_none()))
        .collect();
    for &index in &flow {
        let legs = members_of(index);
        if !legs.is_empty() {
            let inner = legs
                .iter()
                .map(|&m| size.get(m).map_or(0, |s| s.0))
                .max()
                .unwrap_or(0)
                .max(text.get(index).map_or(0, |t| t.chars().count()));
            if let Some(slot) = size.get_mut(index) {
                *slot = (inner + 4, 2 + legs.len() * (box_h + 1));
            }
        }
    }

    // columns over flow nodes; a column shares its widest unit's width
    let col_count = flow.iter().map(|&i| col_of(i) + 1).max().unwrap_or(1);
    let mut col_width = vec![0usize; col_count];
    for &index in &flow {
        if let Some(slot) = col_width.get_mut(col_of(index)) {
            *slot = (*slot).max(size.get(index).map_or(0, |s| s.0));
        }
    }
    let width_of = |col: usize| col_width.get(col).copied().unwrap_or(0);
    let col_x: Vec<usize> = (0..col_count)
        .scan(0usize, |x, col| {
            let here = *x;
            *x += width_of(col) + col_gap;
            Some(here)
        })
        .collect();
    let x_of = |col: usize| col_x.get(col).copied().unwrap_or(0);

    // stack flow units top-to-bottom (cumulative — units vary in height)
    let mut order = flow.clone();
    order.sort_by_key(|&i| ranks.get(i).copied().unwrap_or_default());
    let mut node_box = vec![(0usize, 0usize); model.nodes.len()];
    let mut col_y = vec![0usize; col_count];
    let mut total_h = 0usize;
    for &index in &order {
        let col = col_of(index);
        let y = col_y.get(col).copied().unwrap_or(0);
        if let Some(slot) = node_box.get_mut(index) {
            *slot = (x_of(col), y);
        }
        let h = size.get(index).map_or(box_h, |s| s.1);
        if let Some(slot) = col_y.get_mut(col) {
            *slot = y + h + row_gap.max(1);
        }
        total_h = total_h.max(y + h);
    }
    // position members inside their container
    for &index in &flow {
        let legs = members_of(index);
        let (cx, cy) = node_box.get(index).copied().unwrap_or_default();
        for (k, &m) in legs.iter().enumerate() {
            if let Some(slot) = node_box.get_mut(m) {
                *slot = (cx + 2, cy + 1 + k * (box_h + 1));
            }
        }
    }
    let node_h: Vec<usize> = size.iter().map(|s| s.1).collect();
    let total_w = col_x
        .last()
        .copied()
        .map_or(0, |x| x + col_width.last().copied().unwrap_or(0));

    // two extra rows below carry the return rail for back edges
    let mut grid = Grid::new(total_w, total_h + 2);
    route_edges(
        &mut grid, model, ranks, &col_width, &node_box, &node_h, total_h,
    );

    let mut placements = Vec::with_capacity(model.nodes.len());
    for (index, node) in model.nodes.iter().enumerate() {
        let (x, y) = node_box.get(index).copied().unwrap_or_default();
        let (w, h) = size.get(index).copied().unwrap_or((0, box_h));
        let label = text.get(index).map_or("", String::as_str);
        let is_container = !members_of(index).is_empty();
        if is_container {
            grid.draw_cluster(x, y, w, h, label);
        } else {
            let meta = zoom.show_meta().then(|| status_word(node.status));
            grid.draw_box(x, y, w, h, label, meta);
        }
        placements.push(Placement {
            id: node.id.clone(),
            status: node.status,
            x: u16::try_from(x).unwrap_or(u16::MAX),
            y: u16::try_from(y).unwrap_or(u16::MAX),
            w: u16::try_from(w).unwrap_or(0),
            h: u16::try_from(h).unwrap_or(3),
            container: is_container,
            selectable: node.group.is_none(),
            member: node.group.is_some(),
        });
    }

    Layout {
        lines: grid.into_lines(),
        width: u16::try_from(total_w).unwrap_or(u16::MAX),
        height: u16::try_from(total_h + 2).unwrap_or(u16::MAX),
        placements,
    }
}

/// The box label: the node label plus its status glyph, elided to the zoom's
/// max width (compact) so overview boxes stay small.
fn label_text(label: &str, status: NodeStatus, zoom: Zoom) -> String {
    let mut text = label.to_owned();
    if let Some(max) = zoom.label_max()
        && text.chars().count() > max
    {
        text = text.chars().take(max.saturating_sub(1)).collect::<String>() + "…";
    }
    let glyph = status.glyph();
    if glyph.is_empty() {
        text
    } else {
        format!("{text} {glyph}")
    }
}

fn status_word(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Ok => "success",
        NodeStatus::Failed => "failed",
        NodeStatus::Running => "running",
        NodeStatus::Queued => "queued",
        NodeStatus::Skipped => "skipped",
        NodeStatus::Neutral => "",
    }
}

/// Route every edge: group forward edges by parent into one clean fork each;
/// back edges (cycles) loop under the boxes via the return rail at `rail`.
fn route_edges(
    grid: &mut Grid,
    model: &Model,
    ranks: &[(usize, usize)],
    col_width: &[usize],
    node_box: &[(usize, usize)],
    node_h: &[usize],
    rail: usize,
) {
    let col_of = |index: usize| ranks.get(index).map_or(0, |(c, _)| *c);
    let width_of = |col: usize| col_width.get(col).copied().unwrap_or(0);
    let box_of = |index: usize| node_box.get(index).copied().unwrap_or_default();
    let h_of = |index: usize| node_h.get(index).copied().unwrap_or(1);
    // an edge connects at a node's vertical centre (containers are tall)
    let mid_of = |index: usize| box_of(index).1 + h_of(index) / 2;

    let mut forward: Vec<Vec<usize>> = vec![Vec::new(); model.nodes.len()];
    for edge in &model.edges {
        let (Some(from), Some(to)) = (model.index_of(&edge.from), model.index_of(&edge.to)) else {
            continue;
        };
        if col_of(to) > col_of(from) {
            if let Some(children) = forward.get_mut(from) {
                children.push(to);
            }
        } else {
            route_back_edge(
                grid,
                box_of(from),
                width_of(col_of(from)),
                h_of(from),
                box_of(to),
                width_of(col_of(to)),
                h_of(to),
                rail,
            );
        }
    }
    for (from, children) in forward.iter().enumerate() {
        if children.is_empty() {
            continue;
        }
        let parent = (box_of(from).0, mid_of(from));
        let targets: Vec<(usize, usize)> =
            children.iter().map(|&c| (box_of(c).0, mid_of(c))).collect();
        draw_fork(grid, parent, width_of(col_of(from)), &targets);
    }
}

/// Draw one parent's fan-out as a single fork: a stub out of the parent to a
/// shared channel, one junction there (├ ┬ ┤ …), then a vertical down/up to each
/// child's row and a stub into it with an arrowhead. One junction per parent —
/// no independent crossings, no stray stubs (corners terminate every rail).
/// `targets` are each child's `(left_x, mid_y)`.
fn draw_fork(grid: &mut Grid, parent: (usize, usize), parent_w: usize, targets: &[(usize, usize)]) {
    let sx = parent.0 + parent_w;
    let sy = parent.1;
    let nearest = targets.iter().map(|&(x, _)| x).min().unwrap_or(sx + 2);
    if nearest <= sx + 1 {
        return;
    }
    let channel = sx + (nearest - sx) / 2;
    for x in sx..channel {
        grid.line(x, sy, Dir::L | Dir::R);
    }
    let mut fork = Dir::L;
    for &(cx, cy) in targets {
        let ex = cx.saturating_sub(1);
        match cy.cmp(&sy) {
            Ordering::Equal => {
                fork |= Dir::R;
                for x in channel..ex {
                    grid.line(x, sy, Dir::L | Dir::R);
                }
            }
            Ordering::Greater => {
                fork |= Dir::D;
                for y in sy + 1..cy {
                    grid.line(channel, y, Dir::U | Dir::D);
                }
                grid.line(channel, cy, Dir::U | Dir::R); // ╰
                for x in channel + 1..ex {
                    grid.line(x, cy, Dir::L | Dir::R);
                }
            }
            Ordering::Less => {
                fork |= Dir::U;
                for y in cy + 1..sy {
                    grid.line(channel, y, Dir::U | Dir::D);
                }
                grid.line(channel, cy, Dir::D | Dir::R); // ╭
                for x in channel + 1..ex {
                    grid.line(x, cy, Dir::L | Dir::R);
                }
            }
        }
        grid.put(ex, cy, '▸');
    }
    grid.line(channel, sy, fork);
}

/// A cycle's back edge: down from the parent's bottom to a rail below all boxes,
/// left along the rail, up into the child's bottom (arrow points up).
// endpoints + sizes + rail: a self-contained orthogonal route, not worth a struct
#[allow(clippy::too_many_arguments)]
fn route_back_edge(
    grid: &mut Grid,
    from: (usize, usize),
    from_w: usize,
    from_h: usize,
    to: (usize, usize),
    to_w: usize,
    to_h: usize,
    rail: usize,
) {
    let fx = from.0 + from_w / 2;
    let tx = to.0 + to_w / 2;
    let fy = from.1 + from_h;
    let ty = to.1 + to_h;
    for y in fy..rail {
        grid.line(fx, y, Dir::U | Dir::D);
    }
    grid.line(fx, rail, Dir::U | Dir::L);
    let (lo, hi) = (tx.min(fx), tx.max(fx));
    for x in lo + 1..hi {
        grid.line(x, rail, Dir::L | Dir::R);
    }
    grid.line(tx, rail, Dir::U | Dir::R);
    for y in ty + 1..rail {
        grid.line(tx, y, Dir::U | Dir::D);
    }
    grid.put(tx, ty, '▴');
}

struct Grid {
    cells: Vec<Vec<char>>,
}

/// Direction bits for merging box-drawing line characters at junctions.
struct Dir;
impl Dir {
    const U: u8 = 1;
    const D: u8 = 2;
    const L: u8 = 4;
    const R: u8 = 8;
}

impl Grid {
    fn new(width: usize, height: usize) -> Self {
        Self {
            cells: vec![vec![' '; width]; height],
        }
    }

    fn put(&mut self, x: usize, y: usize, ch: char) {
        if let Some(cell) = self.cells.get_mut(y).and_then(|row| row.get_mut(x)) {
            *cell = ch;
        }
    }

    /// Plot a line segment, merging with any line already there so crossings and
    /// branches render as proper junctions (├ ┬ ┼ …).
    fn line(&mut self, x: usize, y: usize, mask: u8) {
        let Some(cell) = self.cells.get_mut(y).and_then(|row| row.get_mut(x)) else {
            return;
        };
        let merged = char_to_mask(*cell) | mask;
        *cell = mask_to_char(merged);
    }

    /// Draw a node box `box_h` rows tall. `box_h == 1` is the compact overview
    /// form `[ label ]` (no top/bottom rule); taller boxes are rounded outlines
    /// with the label on the first content row and `meta` on the next, if given.
    fn draw_box(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        box_h: usize,
        text: &str,
        meta: Option<&str>,
    ) {
        if w < 2 || box_h == 0 {
            return;
        }
        if box_h == 1 {
            self.put(x, y, '[');
            self.put(x + w - 1, y, ']');
            self.write_centered(x, y, w, text);
            return;
        }
        let bottom = y + box_h - 1;
        self.put(x, y, '╭');
        self.put(x + w - 1, y, '╮');
        self.put(x, bottom, '╰');
        self.put(x + w - 1, bottom, '╯');
        for col in 1..w - 1 {
            self.put(x + col, y, '─');
            self.put(x + col, bottom, '─');
        }
        for row in y + 1..bottom {
            self.put(x, row, '│');
            self.put(x + w - 1, row, '│');
        }
        self.write_centered(x, y + 1, w, text);
        if let Some(meta) = meta
            && box_h >= 4
        {
            self.write_centered(x, y + 2, w, meta);
        }
    }

    /// Draw a group container: a rounded outline with `title` set into the top
    /// border (`╭─ ▾ test ✗ ─╮`). Member boxes are drawn separately inside it.
    fn draw_cluster(&mut self, x: usize, y: usize, w: usize, h: usize, title: &str) {
        if w < 2 || h < 2 {
            return;
        }
        let bottom = y + h - 1;
        self.put(x, y, '╭');
        self.put(x + w - 1, y, '╮');
        self.put(x, bottom, '╰');
        self.put(x + w - 1, bottom, '╯');
        for col in 1..w - 1 {
            self.put(x + col, y, '─');
            self.put(x + col, bottom, '─');
        }
        for row in y + 1..bottom {
            self.put(x, row, '│');
            self.put(x + w - 1, row, '│');
        }
        for (i, ch) in format!(" {title} ").chars().enumerate() {
            if x + 2 + i < x + w - 1 {
                self.put(x + 2 + i, y, ch);
            }
        }
    }

    /// Center `text` within the box interior (`w - 2`) on row `y`.
    fn write_centered(&mut self, x: usize, y: usize, w: usize, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        let pad = (w.saturating_sub(2)).saturating_sub(chars.len()) / 2;
        for (i, ch) in chars.iter().enumerate() {
            if 1 + pad + i < w - 1 {
                self.put(x + 1 + pad + i, y, *ch);
            }
        }
    }

    fn into_lines(self) -> Vec<String> {
        self.cells
            .into_iter()
            .map(|row| row.into_iter().collect::<String>().trim_end().to_owned())
            .collect()
    }
}

fn char_to_mask(ch: char) -> u8 {
    match ch {
        '─' => Dir::L | Dir::R,
        '│' => Dir::U | Dir::D,
        '╭' => Dir::D | Dir::R,
        '╮' => Dir::D | Dir::L,
        '╰' => Dir::U | Dir::R,
        '╯' => Dir::U | Dir::L,
        '├' => Dir::U | Dir::D | Dir::R,
        '┤' => Dir::U | Dir::D | Dir::L,
        '┬' => Dir::D | Dir::L | Dir::R,
        '┴' => Dir::U | Dir::L | Dir::R,
        '┼' => Dir::U | Dir::D | Dir::L | Dir::R,
        _ => 0,
    }
}

fn mask_to_char(mask: u8) -> char {
    match mask {
        m if m == Dir::L | Dir::R => '─',
        m if m == Dir::U | Dir::D => '│',
        m if m == Dir::D | Dir::R => '╭',
        m if m == Dir::D | Dir::L => '╮',
        m if m == Dir::U | Dir::R => '╰',
        m if m == Dir::U | Dir::L => '╯',
        m if m == Dir::U | Dir::D | Dir::R => '├',
        m if m == Dir::U | Dir::D | Dir::L => '┤',
        m if m == Dir::D | Dir::L | Dir::R => '┬',
        m if m == Dir::U | Dir::L | Dir::R => '┴',
        0 => ' ',
        _ => '┼',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layered_draws_boxes_and_places_every_node() {
        let model = Model::demo();
        let layout = Layered.lay_out(&model, Zoom::Normal);
        assert_eq!(layout.placements.len(), model.nodes.len());
        let art = layout.lines.join("\n");
        assert!(
            art.contains('╭') && art.contains('╯'),
            "rounded boxes drawn"
        );
        assert!(art.contains('▸'), "edges have arrowheads");
        // every placement's top-left cell is a box corner
        for p in &layout.placements {
            let row: Vec<char> = layout.lines[p.y as usize].chars().collect();
            assert_eq!(
                row.get(p.x as usize),
                Some(&'╭'),
                "{:?} top-left corner",
                p.id
            );
        }
    }

    #[test]
    fn cyclic_graph_lays_out_without_panicking() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::LeftRight);
        let n = |id: &str| Node::leaf(id, NodeStatus::Neutral);
        model.nodes = vec![n("a"), n("b")];
        let e = |a: &str, b: &str| Edge {
            from: NodeId::new(a),
            to: NodeId::new(b),
            label: None,
        };
        model.edges = vec![e("a", "b"), e("b", "a")];
        assert_eq!(Layered.lay_out(&model, Zoom::Normal).placements.len(), 2);
    }
}
