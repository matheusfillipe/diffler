//! Spike bake-off: render the same model with the `tui-nodes` widget, to judge
//! it against `ascii-dag`. tui-nodes is a self-rendering `StatefulWidget`: it
//! owns layout and draws bordered boxes (with per-node border styles — real
//! status-colored borders) plus its own orthogonal connectors into the area.
//!
//! It does NOT fit the `GraphEngine::lay_out -> cells` seam ascii-dag uses: it
//! draws directly and lays out into the given `area` with no viewport/scroll, so
//! large graphs overflow. That mismatch is itself a bake-off finding — adopting
//! it would mean a render-level seam and giving up our scroll/nav control.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::StatefulWidget;
use tui_nodes::{Connection, NodeGraph, NodeLayout};

use super::model::{Model, NodeStatus};
use crate::theme::Theme;

/// Draw `model` into `area` using tui-nodes. Node borders are colored by status.
pub fn render(model: &Model, theme: &Theme, area: Rect, buf: &mut Buffer) {
    // titles are borrowed for the graph's lifetime; keep them alive here
    let titles: Vec<String> = model.nodes.iter().map(|n| n.label.clone()).collect();
    let nodes: Vec<NodeLayout<'_>> = model
        .nodes
        .iter()
        .zip(&titles)
        .map(|(node, title)| {
            let width = u16::try_from(title.chars().count()).unwrap_or(8) + 2;
            NodeLayout::new((width, 3))
                .with_title(title.as_str())
                .with_border_style(Style::new().fg(status_color(theme, node.status)))
        })
        .collect();

    // each edge gets a fresh port on both endpoints so parallel edges don't
    // collapse onto one connector
    let mut out_port = vec![0usize; model.nodes.len()];
    let mut in_port = vec![0usize; model.nodes.len()];
    let connections: Vec<Connection> = model
        .edges
        .iter()
        .filter_map(|edge| {
            let from = model.index_of(&edge.from)?;
            let to = model.index_of(&edge.to)?;
            let (fp, tp) = (out_port[from], in_port[to]);
            out_port[from] += 1;
            in_port[to] += 1;
            Some(Connection::new(from, fp, to, tp))
        })
        .collect();

    let mut graph = NodeGraph::new(
        nodes,
        connections,
        area.width as usize,
        area.height as usize,
    );
    graph.calculate();
    graph.render(area, buf, &mut ());
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

    #[test]
    fn tui_nodes_renders_the_demo() {
        let model = Model::demo();
        let theme = Theme::github_dark();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(&model, &theme, area, frame.buffer_mut());
            })
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
