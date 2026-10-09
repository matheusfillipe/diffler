//! The engine-agnostic graph model. It allows cycles, so the layout engine
//! decides how to handle back-edges.

use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

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
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Failed => "×",
            Self::Running => "●",
            Self::Queued => "·",
            Self::Skipped => "–",
            Self::Neutral => "",
        }
    }

    /// The more severe of two statuses, so a failing matrix leg colours its
    /// collapsed group.
    #[must_use]
    pub fn worse(self, other: Self) -> Self {
        let rank = |s: Self| match s {
            Self::Failed => 5,
            Self::Running => 4,
            Self::Queued => 3,
            Self::Skipped => 2,
            Self::Neutral => 1,
            Self::Ok => 0,
        };
        if rank(self) >= rank(other) {
            self
        } else {
            other
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RankDir {
    #[default]
    TopDown,
    LeftRight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: NodeId,
    pub label: String,
    pub status: NodeStatus,
    /// The foldable group a member node (a CI matrix leg) belongs to; we hide
    /// it while the group is collapsed.
    pub group: Option<String>,
    /// The group key on a group's root, the one node that folds and that
    /// external edges connect to.
    pub foldable: Option<String>,
    /// The outermost mermaid `subgraph` this node was declared in. It only
    /// affects the outline we draw, never ranking.
    pub subgraph: Option<String>,
    /// A mermaid `{decision}` node, drawn with a `◇` marker since the grid
    /// cannot draw a diamond.
    pub decision: bool,
}

impl Node {
    pub fn leaf(id: &str, status: NodeStatus) -> Self {
        Self {
            id: NodeId::new(id),
            label: id.to_owned(),
            status,
            group: None,
            foldable: None,
            subgraph: None,
            decision: false,
        }
    }

    #[must_use]
    pub fn in_group(mut self, group: &str) -> Self {
        self.group = Some(group.to_owned());
        self
    }

    #[must_use]
    pub fn fold_root(mut self, group: &str) -> Self {
        self.foldable = Some(group.to_owned());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub label: Option<String>,
}

/// A mermaid `subgraph`. We outline its members only when they land
/// contiguous with no foreign node inside their bounding box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subgraph {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Model {
    pub rankdir: RankDir,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub subgraphs: Vec<Subgraph>,
}

impl Model {
    pub fn new(rankdir: RankDir) -> Self {
        Self {
            rankdir,
            nodes: Vec::new(),
            edges: Vec::new(),
            subgraphs: Vec::new(),
        }
    }

    pub fn index_of(&self, id: &NodeId) -> Option<usize> {
        self.nodes.iter().position(|n| &n.id == id)
    }

    pub fn foldable_of(&self, id: &NodeId) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| &n.id == id)
            .and_then(|n| n.foldable.clone())
    }

    /// The model with `collapsed` groups' members and their edges dropped. A
    /// root always takes the worst status of its members.
    #[must_use]
    pub fn collapse(&self, collapsed: &HashSet<String>) -> Model {
        let hidden: HashSet<&NodeId> = self
            .nodes
            .iter()
            .filter(|n| n.group.as_deref().is_some_and(|g| collapsed.contains(g)))
            .map(|n| &n.id)
            .collect();

        let mut out = Model::new(self.rankdir);
        for node in &self.nodes {
            if hidden.contains(&node.id) {
                continue;
            }
            let mut node = node.clone();
            if let Some(group) = node.foldable.clone() {
                let mut worst = node.status;
                let mut count = 0usize;
                for member in &self.nodes {
                    if member.group.as_deref() == Some(group.as_str()) {
                        worst = worst.worse(member.status);
                        count += 1;
                    }
                }
                node.status = worst;
                node.label = if collapsed.contains(&group) {
                    format!("▸ {} ({count})", node.label)
                } else {
                    format!("▾ {}", node.label)
                };
            }
            out.nodes.push(node);
        }
        out.edges = self
            .edges
            .iter()
            .filter(|e| !hidden.contains(&e.from) && !hidden.contains(&e.to))
            .cloned()
            .collect();
        out.subgraphs.clone_from(&self.subgraphs);
        out
    }

    #[cfg(test)]
    pub(crate) fn demo() -> Self {
        use NodeStatus::{Failed, Neutral, Ok, Queued, Running};
        let mut model = Self::new(RankDir::LeftRight);
        model.nodes = vec![
            Node::leaf("lint", Ok),
            Node::leaf("typos", Ok),
            Node::leaf("deny", Ok),
            Node::leaf("test", Neutral).fold_root("test"),
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
            edge("lint", "test"),
            edge("typos", "test"),
            edge("deny", "test"),
            edge("test", "build"),
            edge("build", "publish-crates"),
            edge("build", "publish-npm"),
            edge("build", "publish-aur"),
        ];
        model
    }
}
