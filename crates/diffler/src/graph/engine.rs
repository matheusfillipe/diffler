//! Spike: layout engines behind one trait, so the spike can A/B them on the
//! same model. The view consumes an owned [`Layout`] (no engine lifetimes leak
//! out). `ascii-dag` is the favoured engine (real Sugiyama layout + scroll);
//! `tui-nodes` is added later as the bake-off comparison.

use ascii_dag::graph::Graph;

use super::model::{Model, NodeId, NodeStatus};

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
    fn lay_out(&self, model: &Model) -> Layout;
}

/// `ascii-dag`: Sugiyama layered layout + orthogonal edge routing, rendered to
/// an ASCII grid whose cell coordinates match the layout IR (verified) — so the
/// view can recolor each node's cells from the IR placements.
pub struct AsciiDag;

impl GraphEngine for AsciiDag {
    fn name(&self) -> &'static str {
        "ascii-dag"
    }

    fn lay_out(&self, model: &Model) -> Layout {
        // ascii-dag borrows &str labels for the Graph's lifetime; keep the
        // bracketed labels alive here while we render and lay out.
        let labels: Vec<String> = model
            .nodes
            .iter()
            .map(|n| {
                let glyph = n.status.glyph();
                if glyph.is_empty() {
                    n.label.clone()
                } else {
                    format!("{} {glyph}", n.label)
                }
            })
            .collect();

        let mut dag = Graph::new();
        for (index, label) in labels.iter().enumerate() {
            dag.add_node(index, label.as_str());
        }
        for edge in &model.edges {
            if let (Some(from), Some(to)) = (model.index_of(&edge.from), model.index_of(&edge.to)) {
                dag.add_edge(from, to, None);
            }
        }

        let lines: Vec<String> = dag.render().lines().map(str::to_owned).collect();
        let ir = dag.compute_layout();
        let placements = ir
            .nodes()
            .iter()
            .filter_map(|node| {
                let model_node = model.nodes.get(node.id)?;
                Some(Placement {
                    id: model_node.id.clone(),
                    status: model_node.status,
                    x: u16::try_from(node.x).unwrap_or(u16::MAX),
                    y: u16::try_from(node.y).unwrap_or(u16::MAX),
                    w: u16::try_from(node.width).unwrap_or(0),
                    h: u16::try_from(node.height).unwrap_or(1),
                })
            })
            .collect();

        Layout {
            lines,
            width: u16::try_from(ir.width()).unwrap_or(u16::MAX),
            height: u16::try_from(ir.height()).unwrap_or(u16::MAX),
            placements,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_dag_lays_out_the_demo_with_aligned_placements() {
        let model = Model::demo();
        let layout = AsciiDag.lay_out(&model);
        assert!(!layout.lines.is_empty(), "renders art");
        assert_eq!(layout.placements.len(), model.nodes.len());
        // every placement's cell range must sit inside the rendered grid and
        // land on the node's bracketed label (coordinate alignment)
        for p in &layout.placements {
            let row = layout
                .lines
                .get(p.y as usize)
                .unwrap_or_else(|| panic!("row {} present for {:?}", p.y, p.id));
            let cells: Vec<char> = row.chars().collect();
            let start = p.x as usize;
            assert!(
                cells.get(start) == Some(&'['),
                "placement {:?} starts on a node bracket: row={row:?}",
                p.id
            );
        }
    }

    #[test]
    fn cyclic_graph_lays_out_without_panicking() {
        // mutual recursion: a -> b -> a, plus a self loop — not a DAG
        use super::super::model::{Edge, Node, RankDir};
        let mut model = Model::new(RankDir::TopDown);
        let n = |id: &str| Node {
            id: NodeId::new(id),
            label: id.to_owned(),
            status: NodeStatus::Neutral,
        };
        model.nodes = vec![n("a"), n("b")];
        let e = |a: &str, b: &str| Edge {
            from: NodeId::new(a),
            to: NodeId::new(b),
            label: None,
        };
        model.edges = vec![e("a", "b"), e("b", "a")];
        let layout = AsciiDag.lay_out(&model);
        assert_eq!(layout.placements.len(), 2, "both cyclic nodes placed");
    }
}
