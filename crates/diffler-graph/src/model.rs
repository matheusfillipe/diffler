//! The engine-agnostic graph model. A plain directed graph — cycles are
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

    /// The more severe of two statuses, so a failing matrix leg dominates a
    /// collapsed group's (or aggregate run's) status.
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
    /// Set on a member node (a CI matrix leg): the key of the foldable group it
    /// belongs to. Members hang off their group's root and are hidden when the
    /// group is collapsed. `None` for ordinary nodes and group roots.
    pub group: Option<String>,
    /// Set on the one *root* node of a foldable group: the group key. Only a
    /// root node is foldable (it takes the collapse shortcut); external edges
    /// connect to the root, and its members branch off it.
    pub foldable: Option<String>,
}

impl Node {
    /// An ordinary node whose label is its id.
    pub fn leaf(id: &str, status: NodeStatus) -> Self {
        Self {
            id: NodeId::new(id),
            label: id.to_owned(),
            status,
            group: None,
            foldable: None,
        }
    }

    /// Mark this node a member (leg) of `group`.
    pub fn in_group(mut self, group: &str) -> Self {
        self.group = Some(group.to_owned());
        self
    }

    /// Mark this node the foldable root of `group`.
    pub fn fold_root(mut self, group: &str) -> Self {
        self.foldable = Some(group.to_owned());
        self
    }
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

    /// The foldable group key of a node, if it is a group root. Only roots are
    /// foldable; members and ordinary nodes return `None`.
    pub fn foldable_of(&self, id: &NodeId) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| &n.id == id)
            .and_then(|n| n.foldable.clone())
    }

    /// The render view for a set of collapsed groups. Every foldable root gets a
    /// fold marker (`▾` open / `▸ … (N)` closed); a collapsed group's members and
    /// the edges touching them are dropped, so only the root stays in the flow.
    /// The root's status reflects the worst of its members either way.
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
        out
    }

    /// A CI-shaped sample used by tests and snapshots: lint/typos/deny fan into
    /// a foldable test matrix, which fans into the publish jobs.
    pub fn demo() -> Self {
        use NodeStatus::{Failed, Neutral, Ok, Queued, Running};
        let mut model = Self::new(RankDir::TopDown);
        model.nodes = vec![
            Node::leaf("lint", Ok),
            Node::leaf("typos", Ok),
            Node::leaf("deny", Ok),
            // the test matrix: one foldable root with three legs branching off it
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
            // the flow connects to the group as a whole; the legs live inside
            // its container (by membership), not via edges
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
