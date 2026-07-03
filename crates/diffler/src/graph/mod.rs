//! A navigable orthogonal node-graph component for ratatui, plus the host glue
//! mapping the app theme onto its palette. The component is IO-free (no terminal
//! setup, event loop, or network); the host builds a [`Model`] (from CI, …),
//! pushes it into a [`GraphView`], renders it, and reacts to [`GraphAction`]s.

mod engine;
mod model;
mod theme;
mod view;

pub use engine::{GraphEngine, Layered, Zoom};
pub use model::{Edge, Model, Node, NodeId, NodeStatus, RankDir};
pub use theme::GraphTheme;
pub use view::{Dir, GraphAction, GraphView};

use crate::theme::Theme;

/// Map the app theme onto the component's palette.
pub fn graph_theme(theme: &Theme) -> GraphTheme {
    GraphTheme {
        bg: theme.bg,
        fg: theme.fg,
        dim: theme.dim,
        ok: theme.added,
        failed: theme.error_fg,
        running: theme.warn_fg,
        queued: theme.dim,
        panel: theme.panel,
        search: theme.search,
    }
}
