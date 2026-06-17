//! Spike: the engine-agnostic graph model. A plain directed graph — cycles are
//! allowed (CI pipelines are DAGs, but call/reference maps are not), so layout
//! engines, not the model, decide how to handle back-edges. Front-ends (GitHub
//! Actions today; DOT/mermaid/LSP later) all build this same shape.

use std::collections::HashSet;

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
    /// Members of the same group (e.g. a CI matrix's legs) share this key; a
    /// collapsed group renders as one node. `None` for ungrouped nodes.
    pub group: Option<String>,
}

impl Node {
    /// An ungrouped node whose label is its id.
    pub fn leaf(id: &str, status: NodeStatus) -> Self {
        Self {
            id: NodeId::new(id),
            label: id.to_owned(),
            status,
            group: None,
        }
    }

    pub fn in_group(mut self, group: &str) -> Self {
        self.group = Some(group.to_owned());
        self
    }
}

/// Severity order so a failing matrix leg dominates a collapsed group's status.
fn worse(a: NodeStatus, b: NodeStatus) -> NodeStatus {
    let rank = |s: NodeStatus| match s {
        NodeStatus::Failed => 5,
        NodeStatus::Running => 4,
        NodeStatus::Queued => 3,
        NodeStatus::Skipped => 2,
        NodeStatus::Neutral => 1,
        NodeStatus::Ok => 0,
    };
    if rank(a) >= rank(b) { a } else { b }
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

    /// The group a node belongs to, if any.
    pub fn group_of(&self, id: &NodeId) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| &n.id == id)
            .and_then(|n| n.group.clone())
    }

    /// Whether a group has more than one member (worth collapsing).
    pub fn is_collapsible(&self, group: &str) -> bool {
        self.nodes
            .iter()
            .filter(|n| n.group.as_deref() == Some(group))
            .count()
            > 1
    }

    /// Collapse each named group into a single node (label `group (N)`, worst
    /// member status), rewiring edges to the group and dropping the now-internal
    /// ones. Ungrouped nodes and uncollapsed groups pass through untouched.
    #[must_use]
    pub fn collapse(&self, collapsed: &HashSet<String>) -> Model {
        if collapsed.is_empty() {
            return self.clone();
        }
        let remap = |id: &NodeId| -> NodeId {
            match self.group_of(id) {
                Some(g) if collapsed.contains(&g) => NodeId::new(g),
                _ => id.clone(),
            }
        };
        let mut out = Model::new(self.rankdir);
        let mut emitted: HashSet<String> = HashSet::new();
        for node in &self.nodes {
            match &node.group {
                Some(g) if collapsed.contains(g) => {
                    if emitted.insert(g.clone()) {
                        let members = self.nodes.iter().filter(|n| n.group.as_ref() == Some(g));
                        let count = members.clone().count();
                        let status = members
                            .map(|m| m.status)
                            .reduce(worse)
                            .unwrap_or(NodeStatus::Neutral);
                        out.nodes.push(Node {
                            id: NodeId::new(g.clone()),
                            label: format!("{g} ({count})"),
                            status,
                            group: Some(g.clone()),
                        });
                    }
                }
                _ => out.nodes.push(node.clone()),
            }
        }
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for edge in &self.edges {
            let (from, to) = (remap(&edge.from), remap(&edge.to));
            if from == to {
                continue;
            }
            if seen.insert((from.0.clone(), to.0.clone())) {
                out.edges.push(Edge {
                    from,
                    to,
                    label: edge.label.clone(),
                });
            }
        }
        out
    }

    /// A small CI-shaped sample for the `--demo` path: lint/typos/deny fan into
    /// a test matrix, which fans into the publish jobs — the shape of our own
    /// release pipeline.
    pub fn demo() -> Self {
        use NodeStatus::{Failed, Ok, Queued, Running};
        let mut model = Self::new(RankDir::TopDown);
        model.nodes = vec![
            Node::leaf("lint", Ok),
            Node::leaf("typos", Ok),
            Node::leaf("deny", Ok),
            // the test matrix is a collapsible group
            Node::leaf("test ubuntu", Ok).in_group("test"),
            Node::leaf("test macos", Ok).in_group("test"),
            Node::leaf("test windows", Failed).in_group("test"),
            Node::leaf("build", Running),
            Node::leaf("publish-crates", Queued),
            Node::leaf("publish-npm", Queued),
            Node::leaf("publish-aur", Queued),
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
        model.nodes = [
            "main", "load", "parse", "tokenize", "eval", "apply", "builtin", "render", "error",
        ]
        .into_iter()
        .map(|id| Node::leaf(id, NodeStatus::Neutral))
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
