//! Layout + render engines behind one trait, so the renderer is swappable
//! (ego-centric/radial later) without touching the view.
//! The view consumes an owned [`Layout`] (no engine lifetimes leak out).
//!
//! `Layered` is the favoured engine: longest-path layering assigns each node a
//! column, then it draws the GitHub-style look: outlined rounded boxes laid out
//! left-to-right, wired by clean orthogonal rails.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::graph::model::{Model, NodeId, NodeStatus, RankDir};

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
    /// `g`/`G` only land on these: crossing columns enters a group at its
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

/// Which screen direction a layered pass advances ranks along: `Horizontal`
/// draws the GitHub-style columns left to right, `Vertical` stacks ranks
/// downward instead, which a mermaid `flowchart TD` asks for directly and a
/// `flowchart LR` falls back to when its columns run wider than its card.
/// Sizing (a box's own width and height) never depends on this; only where a
/// rank's extent and a fork's spread land on screen does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    fn of(rankdir: RankDir) -> Self {
        match rankdir {
            RankDir::LeftRight => Self::Horizontal,
            RankDir::TopDown => Self::Vertical,
        }
    }

    /// Real grid `(x, y)` for a point `forward` along the rank axis and
    /// `spread` along the cross axis. The swap is its own inverse, so the
    /// same call recovers `(forward, spread)` from a real `(x, y)`.
    fn xy(self, forward: usize, spread: usize) -> (usize, usize) {
        match self {
            Self::Horizontal => (forward, spread),
            Self::Vertical => (spread, forward),
        }
    }

    /// Direction a rank advances in: right for a horizontal flow, down for a
    /// vertical one.
    fn forward(self) -> u8 {
        match self {
            Self::Horizontal => Dir::R,
            Self::Vertical => Dir::D,
        }
    }

    fn backward(self) -> u8 {
        match self {
            Self::Horizontal => Dir::L,
            Self::Vertical => Dir::U,
        }
    }

    /// Direction a fork spreads its children in, toward a later sibling.
    fn spread_pos(self) -> u8 {
        match self {
            Self::Horizontal => Dir::D,
            Self::Vertical => Dir::R,
        }
    }

    fn spread_neg(self) -> u8 {
        match self {
            Self::Horizontal => Dir::U,
            Self::Vertical => Dir::L,
        }
    }

    /// A forward edge's arrowhead, entering a child from its near edge.
    fn forward_arrow(self) -> char {
        match self {
            Self::Horizontal => '▸',
            // the half-height `▾` sits on the text baseline and reads as a
            // speck at the end of a vertical rail, where `▼` fills the cell
            Self::Vertical => '▼',
        }
    }

    /// A back edge's arrowhead: it always re-enters the target from the rail
    /// side, moving in the spread-negative direction.
    fn spread_neg_arrow(self) -> char {
        match self {
            Self::Horizontal => '▴',
            Self::Vertical => '◂',
        }
    }
}

/// GitHub-style layered renderer: longest-path layering ranks the nodes, then
/// draws rounded outlined boxes and routes orthogonal rails between them,
/// along whichever [`Axis`] the model's [`RankDir`] names.
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
/// Only flow nodes (ordinary + group roots) are ranked: group members live
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
// route, draw: splitting it would only scatter the shared coordinate maps
#[allow(clippy::too_many_lines)]
fn place_and_draw(model: &Model, ranks: &[(usize, usize)], zoom: Zoom) -> Layout {
    let (box_h, row_gap, col_gap) = zoom.metrics();
    // the gaps are named for a left-to-right flow: going down, a rank step is a
    // row and the spread between siblings is a column, so the two trade places
    let (forward_gap, cross_gap) = match Axis::of(model.rankdir) {
        Axis::Horizontal => (col_gap, row_gap),
        Axis::Vertical => (row_gap.max(1) + 2, col_gap),
    };
    let col_of = |index: usize| ranks.get(index).map_or(0, |(c, _)| *c);

    let text: Vec<Vec<String>> = model
        .nodes
        .iter()
        .map(|n| label_lines(&n.label, n.status, zoom))
        .collect();
    let line_width = |lines: &[String]| lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
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
    for (index, lines) in text.iter().enumerate() {
        if let Some(slot) = size.get_mut(index) {
            *slot = (line_width(lines) + 4, box_height(lines.len(), box_h));
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
                .max(text.get(index).map_or(0, |t| line_width(t)));
            if let Some(slot) = size.get_mut(index) {
                *slot = (inner + 4, 2 + legs.len() * (box_h + 1));
            }
        }
    }

    let axis = Axis::of(model.rankdir);
    let size_of = |index: usize| size.get(index).copied().unwrap_or_default();

    // ranks over flow nodes; a rank shares its widest unit's extent along the
    // rank axis (width for a horizontal flow, height for a vertical one)
    let rank_count = flow.iter().map(|&i| col_of(i) + 1).max().unwrap_or(1);
    let mut rank_extent = vec![0usize; rank_count];
    for &index in &flow {
        let (w, h) = size_of(index);
        let (forward, _) = axis.xy(w, h);
        if let Some(slot) = rank_extent.get_mut(col_of(index)) {
            *slot = (*slot).max(forward);
        }
    }
    let rank_pos: Vec<usize> = (0..rank_count)
        .scan(0usize, |p, r| {
            let here = *p;
            *p += rank_extent.get(r).copied().unwrap_or(0) + forward_gap;
            Some(here)
        })
        .collect();

    // stack each rank's units along the cross axis (cumulative, units vary)
    let mut order = flow.clone();
    order.sort_by_key(|&i| ranks.get(i).copied().unwrap_or_default());
    let mut node_box = vec![(0usize, 0usize); model.nodes.len()];
    let mut rank_cursor = vec![0usize; rank_count];
    let mut total_secondary = 0usize;
    for &index in &order {
        let rank = col_of(index);
        let cursor = rank_cursor.get(rank).copied().unwrap_or(0);
        let forward = rank_pos.get(rank).copied().unwrap_or(0);
        if let Some(slot) = node_box.get_mut(index) {
            *slot = axis.xy(forward, cursor);
        }
        let (w, h) = size_of(index);
        let (_, spread) = axis.xy(w, h);
        if let Some(slot) = rank_cursor.get_mut(rank) {
            *slot = cursor + spread + cross_gap.max(1);
        }
        total_secondary = total_secondary.max(cursor + spread);
    }
    centre_ranks(
        &order,
        &rank_cursor,
        &mut node_box,
        col_of,
        axis,
        cross_gap.max(1),
        total_secondary,
    );

    // position members inside their container: a screen-relative offset
    // (right, then down), the same regardless of the flow's own axis. Every
    // leg is spaced by the flat box_h, not its own wrapped height: only
    // mermaid labels wrap and only CI nodes are members, so no member's
    // label wraps today, but one that did would overlap its neighbour here.
    for &index in &flow {
        let legs = members_of(index);
        let (cx, cy) = node_box.get(index).copied().unwrap_or_default();
        for (k, &m) in legs.iter().enumerate() {
            if let Some(slot) = node_box.get_mut(m) {
                *slot = (cx + 2, cy + 1 + k * (box_h + 1));
            }
        }
    }
    let total_primary =
        rank_pos.last().copied().unwrap_or(0) + rank_extent.last().copied().unwrap_or(0);

    // two extra cells past the cross axis carry the return rail for back edges
    let (grid_w, grid_h) = axis.xy(total_primary, total_secondary + 2);
    let mut grid = Grid::new(grid_w, grid_h);
    route_edges(
        &mut grid,
        axis,
        model,
        ranks,
        &rank_extent,
        &node_box,
        &size,
        total_secondary,
    );

    let mut placements = Vec::with_capacity(model.nodes.len());
    for (index, node) in model.nodes.iter().enumerate() {
        let (x, y) = node_box.get(index).copied().unwrap_or_default();
        let (w, h) = size.get(index).copied().unwrap_or((0, box_h));
        let empty = Vec::new();
        let lines = text.get(index).unwrap_or(&empty);
        let is_container = !members_of(index).is_empty();
        if is_container {
            // a foldable group's root is a CI concept, never a mermaid one,
            // so its title is always one line
            let title = lines.first().map_or("", String::as_str);
            grid.draw_cluster(x, y, w, h, title);
        } else {
            let meta = zoom.show_meta().then(|| status_word(node.status));
            grid.draw_box(x, y, w, h, lines, meta);
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
        width: u16::try_from(grid_w).unwrap_or(u16::MAX),
        height: u16::try_from(grid_h).unwrap_or(u16::MAX),
        placements,
    }
}

/// The box label as its drawn lines: the node label's own `\n`-separated
/// lines (mermaid's `<br>`), each elided to the zoom's max width (compact
/// only) so overview boxes stay small, with the status glyph on the last one.
fn label_lines(label: &str, status: NodeStatus, zoom: Zoom) -> Vec<String> {
    let mut lines: Vec<String> = label.split('\n').map(|line| elide(line, zoom)).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let glyph = status.glyph();
    if !glyph.is_empty()
        && let Some(last) = lines.last_mut()
    {
        last.push(' ');
        last.push_str(glyph);
    }
    lines
}

fn elide(line: &str, zoom: Zoom) -> String {
    let Some(max) = zoom.label_max() else {
        return line.to_owned();
    };
    if line.chars().count() <= max {
        return line.to_owned();
    }
    line.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

/// A box's total row count for `lines` content rows, given the zoom's
/// baseline `box_h` for a single-line label (1 = compact bracket, 3 = a
/// bordered box, 4 = bordered with a status-word row). Every extra line
/// costs one more row; a multi-line label always gets at least a minimal
/// bordered box, since the borderless compact style has nowhere to put a
/// second line.
fn box_height(lines: usize, box_h: usize) -> usize {
    let overhead = box_h.saturating_sub(1);
    let overhead = if lines > 1 { overhead.max(2) } else { overhead };
    lines.max(1) + overhead
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
/// back edges (cycles) loop past the boxes via the return rail at `rail`, a
/// cross-axis coordinate beyond every one of them.
// geometry inputs for one routing pass: a self-contained set, not worth a struct
#[allow(clippy::too_many_arguments)]
fn route_edges(
    grid: &mut Grid,
    axis: Axis,
    model: &Model,
    ranks: &[(usize, usize)],
    rank_extent: &[usize],
    node_box: &[(usize, usize)],
    node_size: &[(usize, usize)],
    rail: usize,
) {
    let col_of = |index: usize| ranks.get(index).map_or(0, |(c, _)| *c);
    let rank_forward = |col: usize| rank_extent.get(col).copied().unwrap_or(0);
    let fwd_spread = |index: usize| {
        let (x, y) = node_box.get(index).copied().unwrap_or_default();
        axis.xy(x, y)
    };
    let own_extent = |index: usize| {
        let (w, h) = node_size.get(index).copied().unwrap_or_default();
        axis.xy(w, h)
    };
    // an edge connects at a node's cross-axis centre (containers are tall)
    let spread_center = |index: usize| fwd_spread(index).1 + own_extent(index).1 / 2;

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
                axis,
                fwd_spread(from),
                rank_forward(col_of(from)),
                own_extent(from).1,
                fwd_spread(to),
                rank_forward(col_of(to)),
                own_extent(to).1,
                rail,
            );
        }
    }
    for (from, children) in forward.iter().enumerate() {
        if children.is_empty() {
            continue;
        }
        let parent = (fwd_spread(from).0, spread_center(from));
        let targets: Vec<(usize, usize)> = children
            .iter()
            .map(|&c| (fwd_spread(c).0, spread_center(c)))
            .collect();
        draw_fork(grid, axis, parent, rank_forward(col_of(from)), &targets);
    }
}

/// Draw one parent's fan-out as a single fork: a stub out of the parent to a
/// shared channel, one junction there (├ ┬ ┤ …), then a cross-axis jog to
/// each child's rank and a stub into it with an arrowhead. One junction per
/// parent: no independent crossings, no stray stubs (corners terminate every
/// rail). `parent`/`targets` are `(forward, spread)` points.
fn draw_fork(
    grid: &mut Grid,
    axis: Axis,
    parent: (usize, usize),
    parent_extent: usize,
    targets: &[(usize, usize)],
) {
    let sp = parent.0 + parent_extent;
    let ss = parent.1;
    let nearest = targets.iter().map(|&(p, _)| p).min().unwrap_or(sp + 2);
    if nearest <= sp + 1 {
        return;
    }
    let channel = sp + (nearest - sp) / 2;
    for p in sp..channel {
        grid.line_on(axis, p, ss, axis.forward() | axis.backward());
    }
    let mut fork = axis.backward();
    for &(cp, cs) in targets {
        let ep = cp.saturating_sub(1);
        match cs.cmp(&ss) {
            Ordering::Equal => {
                fork |= axis.forward();
                for p in channel..ep {
                    grid.line_on(axis, p, ss, axis.forward() | axis.backward());
                }
            }
            Ordering::Greater => {
                fork |= axis.spread_pos();
                for s in ss + 1..cs {
                    grid.line_on(axis, channel, s, axis.spread_pos() | axis.spread_neg());
                }
                grid.line_on(axis, channel, cs, axis.spread_neg() | axis.forward()); // ╰ (or its axis rotation)
                for p in channel + 1..ep {
                    grid.line_on(axis, p, cs, axis.forward() | axis.backward());
                }
            }
            Ordering::Less => {
                fork |= axis.spread_neg();
                for s in cs + 1..ss {
                    grid.line_on(axis, channel, s, axis.spread_pos() | axis.spread_neg());
                }
                grid.line_on(axis, channel, cs, axis.spread_pos() | axis.forward()); // ╭ (or its axis rotation)
                for p in channel + 1..ep {
                    grid.line_on(axis, p, cs, axis.forward() | axis.backward());
                }
            }
        }
        grid.put_on(axis, ep, cs, axis.forward_arrow());
    }
    grid.line_on(axis, channel, ss, fork);
}

/// A rank's boxes vary in size, so their centres only line up when each rank is
/// centred on the widest one. An edge joins two centres, so off-centre ranks
/// make every edge leave its box, jog sideways and come back.
fn centre_ranks(
    order: &[usize],
    rank_cursor: &[usize],
    node_box: &mut [(usize, usize)],
    rank_of: impl Fn(usize) -> usize,
    axis: Axis,
    cross_gap: usize,
    total: usize,
) {
    for &index in order {
        let used = rank_cursor
            .get(rank_of(index))
            .copied()
            .unwrap_or(0)
            .saturating_sub(cross_gap);
        // we halve each side, so two boxes whose widths differ by an odd
        // cell still land on the same centre line
        let shift = (total / 2).saturating_sub(used / 2);
        if let Some((x, y)) = node_box.get_mut(index) {
            let (forward, spread) = axis.xy(*x, *y);
            (*x, *y) = axis.xy(forward, spread + shift);
        }
    }
}

/// A cycle's back edge: from the parent's far cross-axis edge to a rail past
/// every box, along the rail, back into the child's far cross-axis edge.
/// `from`/`to` are `(forward, spread)` points at each box's near corner.
// endpoints + sizes + rail: a self-contained orthogonal route, not worth a struct
#[allow(clippy::too_many_arguments)]
fn route_back_edge(
    grid: &mut Grid,
    axis: Axis,
    from: (usize, usize),
    from_fwd_extent: usize,
    from_spread_extent: usize,
    to: (usize, usize),
    to_fwd_extent: usize,
    to_spread_extent: usize,
    rail: usize,
) {
    let ff = from.0 + from_fwd_extent / 2;
    let tf = to.0 + to_fwd_extent / 2;
    let fs = from.1 + from_spread_extent;
    let ts = to.1 + to_spread_extent;
    for s in fs..rail {
        grid.line_on(axis, ff, s, axis.spread_pos() | axis.spread_neg());
    }
    grid.line_on(axis, ff, rail, axis.spread_neg() | axis.backward());
    let (lo, hi) = (tf.min(ff), tf.max(ff));
    for f in lo + 1..hi {
        grid.line_on(axis, f, rail, axis.forward() | axis.backward());
    }
    grid.line_on(axis, tf, rail, axis.spread_neg() | axis.forward());
    for s in ts + 1..rail {
        grid.line_on(axis, tf, s, axis.spread_pos() | axis.spread_neg());
    }
    grid.put_on(axis, tf, ts, axis.spread_neg_arrow());
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

    /// [`Self::line`] at a rank-axis `forward`/cross-axis `spread` point,
    /// converted to a real cell through `axis`.
    fn line_on(&mut self, axis: Axis, forward: usize, spread: usize, mask: u8) {
        let (x, y) = axis.xy(forward, spread);
        self.line(x, y, mask);
    }

    /// [`Self::put`] at a rank-axis `forward`/cross-axis `spread` point.
    fn put_on(&mut self, axis: Axis, forward: usize, spread: usize, ch: char) {
        let (x, y) = axis.xy(forward, spread);
        self.put(x, y, ch);
    }

    /// Draw a node box `box_h` rows tall. `box_h == 1` is the compact overview
    /// form `[ label ]` (no top/bottom rule, one line only); taller boxes are
    /// rounded outlines with `lines` filling the content rows top to bottom
    /// and `meta`, if given, on the row after them.
    fn draw_box(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        box_h: usize,
        lines: &[String],
        meta: Option<&str>,
    ) {
        if w < 2 || box_h == 0 {
            return;
        }
        if box_h == 1 {
            self.put(x, y, '[');
            self.put(x + w - 1, y, ']');
            self.write_centered(x, y, w, lines.first().map_or("", String::as_str));
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
        for (i, line) in lines.iter().enumerate() {
            let row = y + 1 + i;
            if row >= bottom {
                break;
            }
            self.write_centered(x, row, w, line);
        }
        if let Some(meta) = meta {
            let row = y + 1 + lines.len();
            if row < bottom {
                self.write_centered(x, row, w, meta);
            }
        }
    }

    /// Draw a group container: a rounded outline with `title` set into the top
    /// border (`╭─ ▾ test × ─╮`). Member boxes are drawn separately inside it.
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

    /// A `\n` in a label (mermaid's own `<br>`) draws as a second line and
    /// grows the box by one row; a label with none draws exactly as before.
    #[test]
    fn a_multi_line_label_grows_the_box_by_its_extra_lines() {
        use crate::graph::model::{Node, RankDir};
        let mut model = Model::new(RankDir::LeftRight);
        model.nodes = vec![Node::leaf("one", NodeStatus::Neutral), {
            let mut two = Node::leaf("two", NodeStatus::Neutral);
            two.label = "first\nsecond".to_owned();
            two
        }];
        let layout = Layered.lay_out(&model, Zoom::Normal);
        let height_of = |id: &str| {
            layout
                .placements
                .iter()
                .find(|p| p.id.0 == id)
                .expect("placed")
                .h
        };
        assert_eq!(
            height_of("two"),
            height_of("one") + 1,
            "one extra content row"
        );
        let art = layout.lines.join("\n");
        assert!(art.contains("first"), "{art}");
        assert!(art.contains("second"), "{art}");
    }

    /// `RankDir::TopDown` stacks ranks downward: a chain's boxes share a
    /// column and grow down the `y` axis.
    #[test]
    fn top_down_stacks_ranks_by_row_not_column() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::TopDown);
        let n = |id: &str| Node::leaf(id, NodeStatus::Neutral);
        model.nodes = vec![n("a"), n("b"), n("c")];
        let e = |a: &str, b: &str| Edge {
            from: NodeId::new(a),
            to: NodeId::new(b),
            label: None,
        };
        model.edges = vec![e("a", "b"), e("b", "c")];
        let layout = Layered.lay_out(&model, Zoom::Normal);
        let at = |id: &str| {
            layout
                .placements
                .iter()
                .find(|p| p.id.0 == id)
                .expect("placed")
        };
        let (first, second, third) = (at("a"), at("b"), at("c"));
        assert_eq!(first.x, second.x, "a chain shares a column top-down");
        assert_eq!(second.x, third.x);
        assert!(
            first.y < second.y && second.y < third.y,
            "each rank sits below the last"
        );
        let art = layout.lines.join("\n");
        assert!(art.contains('▼'), "the fork points down, not right: {art}");
    }

    /// Going down, a rank step costs rows, so it takes the small gap and the
    /// spread between siblings takes the wide one. A five-step chain then fits
    /// the rows a card gives a figure.
    #[test]
    fn a_chain_drawn_downward_fits_the_rows_a_card_gives_it() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::TopDown);
        let ids = ["a", "b", "c", "d", "e"];
        model.nodes = ids
            .iter()
            .map(|id| Node::leaf(id, NodeStatus::Neutral))
            .collect();
        model.edges = ids
            .windows(2)
            .map(|pair| Edge {
                from: NodeId::new(pair[0]),
                to: NodeId::new(pair[1]),
                label: None,
            })
            .collect();

        let layout = Layered.lay_out(&model, Zoom::Normal);

        let height = u16::try_from(layout.lines.len()).expect("a short figure");
        assert!(
            height <= crate::app::walkthrough::FIGURE_MAX_ROWS,
            "five steps take {height} rows"
        );
        assert!(
            layout.lines.join("\n").contains('▼'),
            "each step keeps its arrow"
        );
    }

    /// Boxes in a chain differ in width, so their centres only meet when every
    /// rank is centred: an off-centre rank makes each edge leave its box, jog
    /// sideways and come back.
    #[test]
    fn a_downward_chain_of_uneven_boxes_draws_straight() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::TopDown);
        let widths = ["a much wider label here", "narrow", "a medium label"];
        model.nodes = widths
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let mut node = Node::leaf(&format!("n{index}"), NodeStatus::Neutral);
                node.label = (*text).to_owned();
                node
            })
            .collect();
        model.edges = (0..2)
            .map(|index| Edge {
                from: NodeId::new(format!("n{index}")),
                to: NodeId::new(format!("n{}", index + 1)),
                label: None,
            })
            .collect();

        let layout = Layered.lay_out(&model, Zoom::Normal);

        // a box's own sides are `│` too, so only the rows between boxes count
        let rails: Vec<usize> = layout
            .lines
            .iter()
            .filter(|line| line.trim().chars().all(|c| c == '│' || c == '▼'))
            .filter_map(|line| line.find(['│', '▼']))
            .collect();
        assert!(
            rails.windows(2).all(|pair| pair[0] == pair[1]),
            "every rail sits in one column: {rails:?}"
        );
    }

    /// The left-to-right mirror: a wrapped label varies a box's height, so
    /// centring has to run for `Axis::Horizontal` too, keeping a horizontal
    /// chain's rails level.
    #[test]
    fn a_left_to_right_chain_of_uneven_boxes_draws_straight() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::LeftRight);
        let heights = ["one\ntwo\nthree", "single", "one\ntwo"];
        model.nodes = heights
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let mut node = Node::leaf(&format!("n{index}"), NodeStatus::Neutral);
                node.label = (*text).to_owned();
                node
            })
            .collect();
        model.edges = (0..2)
            .map(|index| Edge {
                from: NodeId::new(format!("n{index}")),
                to: NodeId::new(format!("n{}", index + 1)),
                label: None,
            })
            .collect();

        let layout = Layered.lay_out(&model, Zoom::Normal);

        // one `▸` per edge; centred boxes put every one on the same row
        let rows: Vec<usize> = layout
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains('▸'))
            .map(|(y, _)| y)
            .collect();
        assert!(
            rows.windows(2).all(|pair| pair[0] == pair[1]),
            "every rail sits in one row: {rows:?}"
        );
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

    #[test]
    fn cyclic_graph_lays_out_top_down_without_panicking() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::TopDown);
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

    #[test]
    fn rank_nodes_assigns_columns_by_longest_path_and_rows_by_declaration_order() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::LeftRight);
        let n = |id: &str| Node::leaf(id, NodeStatus::Neutral);
        // d has no edges at all; c has two parents at different depths (via b,
        // and directly from d) so it must take the longer of the two
        model.nodes = vec![n("a"), n("b"), n("c"), n("d")];
        let e = |a: &str, b: &str| Edge {
            from: NodeId::new(a),
            to: NodeId::new(b),
            label: None,
        };
        model.edges = vec![e("a", "b"), e("b", "c"), e("d", "c")];
        let ranks = rank_nodes(&model);
        assert_eq!(ranks[0], (0, 0), "a starts the flow");
        assert_eq!(ranks[1], (1, 0), "b is one hop from a");
        assert_eq!(ranks[2], (2, 0), "c takes the longer path in via b, not d");
        assert_eq!(
            ranks[3],
            (0, 1),
            "d has no predecessors so it shares column 0, as the 2nd node placed there"
        );
    }

    #[test]
    fn rank_nodes_treats_a_cycle_back_edge_as_zero_depth() {
        use crate::graph::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::LeftRight);
        model.nodes = vec![
            Node::leaf("a", NodeStatus::Neutral),
            Node::leaf("b", NodeStatus::Neutral),
        ];
        let e = |a: &str, b: &str| Edge {
            from: NodeId::new(a),
            to: NodeId::new(b),
            label: None,
        };
        model.edges = vec![e("a", "b"), e("b", "a")];
        let ranks = rank_nodes(&model);
        // a is visited first: descending into b's predecessor (a itself,
        // already on the path) contributes no depth, so b lands at 0 and a's
        // hop through it lands at 1: the cycle never inflates either depth
        assert_eq!(ranks[0], (1, 0), "a: one real hop through b");
        assert_eq!(
            ranks[1],
            (0, 0),
            "b: its back-edge to a on the path adds no depth"
        );
    }

    #[test]
    fn draw_fork_draws_one_junction_with_a_stub_and_arrow_per_child() {
        // a parent box ending at x=2, mid-row 2, forking to a child level with
        // it (row 2) and a child two rows below (row 6), the level+drop mix
        // that a single junction has to carry
        let mut grid = Grid::new(12, 8);
        draw_fork(&mut grid, Axis::Horizontal, (0, 2), 2, &[(8, 2), (8, 6)]);
        let lines = grid.into_lines();
        let cell = |x: usize, y: usize| lines[y].chars().nth(x).unwrap_or(' ');
        assert_eq!(cell(5, 2), '┬', "one junction carries both branches");
        assert_eq!(cell(7, 2), '▸', "the level child gets a stub arrow");
        assert_eq!(
            cell(5, 6),
            '╰',
            "the dropping branch turns into the child's row"
        );
        assert_eq!(cell(7, 6), '▸', "the dropped child gets a stub arrow too");
        assert_eq!(
            cell(5, 4),
            '│',
            "a vertical rail links the junction to the drop"
        );
    }

    #[test]
    fn route_back_edge_loops_under_boxes_into_the_child_bottom() {
        // source box at (6,0), target box at (0,0), both 2x2, rail at y=5
        let mut grid = Grid::new(10, 7);
        route_back_edge(&mut grid, Axis::Horizontal, (6, 0), 2, 2, (0, 0), 2, 2, 5);
        let lines = grid.into_lines();
        let cell = |x: usize, y: usize| lines[y].chars().nth(x).unwrap_or(' ');
        assert_eq!(cell(7, 2), '│', "descent from the source's bottom");
        assert_eq!(cell(7, 5), '╯', "turn from the descent onto the rail");
        assert_eq!(cell(4, 5), '─', "the rail runs under both boxes");
        assert_eq!(
            cell(1, 5),
            '╰',
            "turn from the rail up into the target's column"
        );
        assert_eq!(cell(1, 3), '│', "ascent back up to the target");
        assert_eq!(cell(1, 2), '▴', "the arrowhead points back into the target");
    }

    /// The same route, top-down: the rail runs beside the ranks (a column,
    /// not a row below them), since the boxes' forward axis is now `y`.
    #[test]
    fn route_back_edge_top_down_loops_beside_the_ranks() {
        let mut grid = Grid::new(7, 10);
        route_back_edge(&mut grid, Axis::Vertical, (6, 0), 2, 2, (0, 0), 2, 2, 5);
        let lines = grid.into_lines();
        let cell = |x: usize, y: usize| lines[y].chars().nth(x).unwrap_or(' ');
        assert_eq!(cell(2, 7), '─', "sideways from the source's far edge");
        assert_eq!(cell(5, 7), '╯', "turn from that run onto the rail");
        assert_eq!(cell(5, 4), '│', "the rail runs beside both ranks");
        assert_eq!(
            cell(5, 1),
            '╮',
            "turn from the rail sideways into the target's row"
        );
        assert_eq!(cell(3, 1), '─', "sideways back to the target");
        assert_eq!(cell(2, 1), '◂', "the arrowhead points back into the target");
    }

    #[test]
    fn box_drawing_masks_round_trip_through_their_glyphs() {
        for ch in ['─', '│', '╭', '╮', '╰', '╯', '├', '┤', '┬', '┴', '┼'] {
            assert_eq!(mask_to_char(char_to_mask(ch)), ch, "{ch} round-trips");
        }
        assert_eq!(
            char_to_mask(' '),
            0,
            "an unrecognized glyph carries no lines"
        );
        assert_eq!(mask_to_char(0), ' ', "no lines draws a blank cell");
    }

    #[test]
    fn grid_line_merges_crossing_segments_into_a_junction() {
        let mut grid = Grid::new(5, 5);
        grid.line(2, 2, Dir::L | Dir::R);
        grid.line(2, 2, Dir::U | Dir::D);
        let lines = grid.into_lines();
        assert_eq!(
            lines[2].chars().nth(2),
            Some('┼'),
            "a horizontal and vertical segment sharing a cell merge into a 4-way junction"
        );
    }
}
