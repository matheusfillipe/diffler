//! A navigable orthogonal node-graph component for ratatui terminal UIs.
//!
//! The host builds a [`Model`] (from CI, DOT, mermaid, LSP, …), pushes it into a
//! [`GraphView`], renders the view into any area, and reacts to the
//! [`GraphAction`]s it emits. The component is IO-free: no terminal setup, no
//! event loop, no network — sources and side effects belong to the host.

mod engine;
mod model;
mod theme;
mod view;

pub use engine::{GraphEngine, Layered, Zoom};
pub use model::{Edge, Model, Node, NodeId, NodeStatus, RankDir};
pub use theme::GraphTheme;
pub use view::{GraphAction, GraphView};
