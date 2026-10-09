//! A navigable orthogonal node-graph component for ratatui. It does no IO: the
//! host builds a [`Model`], pushes it into a [`GraphView`], renders it, and
//! handles the [`GraphAction`]s it returns.

mod callstack;
mod drawing;
mod engine;
pub mod mermaid;
mod model;
mod sequence;
mod text_figure;
mod theme;
mod view;

pub use drawing::{Drawing, FenceKind, FigureResult, figure, validate_fence};
pub use engine::{GraphEngine, Layered, Zoom};
pub use mermaid::{Figure, MermaidError};
pub use model::{Edge, Model, Node, NodeId, NodeStatus, RankDir, Subgraph};
pub use theme::GraphTheme;
pub use view::{Dir, Fit, GraphAction, GraphView};

use crate::theme::Theme;

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
