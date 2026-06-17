//! Spike: the engine-agnostic graph model. A plain directed graph — cycles are
//! allowed (CI pipelines are DAGs, but call/reference maps are not), so layout
//! engines, not the model, decide how to handle back-edges. Front-ends (GitHub
//! Actions today; DOT/mermaid/LSP later) all build this same shape.

/// Stable node key from the source (a CI job name, a DOT id, a symbol path).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Status of a node, driving its color and glyph. Maps from CI job conclusions
/// now; generic enough for other producers later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Ok,
    Failed,
    Running,
    Queued,
    Skipped,
    Neutral,
}

impl NodeStatus {
    /// A compact status glyph shown beside the label.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Failed => "✗",
            Self::Running => "●",
            Self::Queued => "·",
            Self::Skipped => "–",
            Self::Neutral => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankDir {
    TopDown,
    LeftRight,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub label: String,
    pub status: NodeStatus,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub rankdir: RankDir,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Model {
    pub fn new(rankdir: RankDir) -> Self {
        Self {
            rankdir,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Index of a node by id, for engines that key on position.
    pub fn index_of(&self, id: &NodeId) -> Option<usize> {
        self.nodes.iter().position(|n| &n.id == id)
    }

    /// A small CI-shaped sample for the `--demo` path: lint/typos/deny fan into
    /// a test matrix, which fans into the publish jobs — the shape of our own
    /// release pipeline.
    pub fn demo() -> Self {
        use NodeStatus::{Failed, Ok, Queued, Running};
        let mut model = Self::new(RankDir::TopDown);
        let node = |id: &str, status| Node {
            id: NodeId::new(id),
            label: id.to_owned(),
            status,
        };
        model.nodes = vec![
            node("lint", Ok),
            node("typos", Ok),
            node("deny", Ok),
            node("test ubuntu", Ok),
            node("test macos", Ok),
            node("test windows", Failed),
            node("build", Running),
            node("publish-crates", Queued),
            node("publish-npm", Queued),
            node("publish-aur", Queued),
        ];
        let edge = |from: &str, to: &str| Edge {
            from: NodeId::new(from),
            to: NodeId::new(to),
            label: None,
        };
        model.edges = vec![
            edge("lint", "test ubuntu"),
            edge("typos", "test ubuntu"),
            edge("deny", "test macos"),
            edge("lint", "test macos"),
            edge("lint", "test windows"),
            edge("test ubuntu", "build"),
            edge("test macos", "build"),
            edge("test windows", "build"),
            edge("build", "publish-crates"),
            edge("build", "publish-npm"),
            edge("build", "publish-aur"),
        ];
        model
    }

    /// A small call/reference graph for the `--code` path: not a DAG —
    /// `eval`/`apply` are mutually recursive — so it exercises back-edge
    /// routing. Status is `Neutral` (code graphs have no run state).
    pub fn code_demo() -> Self {
        let mut model = Self::new(RankDir::LeftRight);
        let node = |id: &str| Node {
            id: NodeId::new(id),
            label: id.to_owned(),
            status: NodeStatus::Neutral,
        };
        model.nodes = [
            "main", "load", "parse", "tokenize", "eval", "apply", "builtin", "render", "error",
        ]
        .into_iter()
        .map(node)
        .collect();
        let edge = |from: &str, to: &str| Edge {
            from: NodeId::new(from),
            to: NodeId::new(to),
            label: None,
        };
        model.edges = vec![
            edge("main", "load"),
            edge("main", "eval"),
            edge("main", "render"),
            edge("load", "parse"),
            edge("parse", "tokenize"),
            edge("parse", "error"),
            edge("eval", "apply"),
            edge("apply", "eval"), // cycle: mutual recursion
            edge("apply", "builtin"),
            edge("eval", "builtin"),
            edge("render", "error"),
            edge("builtin", "error"),
        ];
        model
    }
}
