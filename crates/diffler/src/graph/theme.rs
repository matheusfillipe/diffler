//! The colors the graph renderer needs, supplied by the host so the component
//! stays independent of any particular app's theme.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct GraphTheme {
    /// Default background and node text/foreground.
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    /// Status colors for node borders/labels.
    pub ok: Color,
    pub failed: Color,
    pub running: Color,
    pub queued: Color,
    /// Bottom-bar / panel background.
    pub panel: Color,
}

impl GraphTheme {
    /// The color for a node status.
    pub(crate) fn status(&self, status: crate::graph::model::NodeStatus) -> Color {
        use crate::graph::model::NodeStatus;
        match status {
            NodeStatus::Ok => self.ok,
            NodeStatus::Failed => self.failed,
            NodeStatus::Running => self.running,
            NodeStatus::Queued | NodeStatus::Skipped => self.queued,
            NodeStatus::Neutral => self.fg,
        }
    }
}
