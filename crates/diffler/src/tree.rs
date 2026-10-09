//! Sidebar rows and the directory-trie flattening behind them, with no
//! rendering or app state so the navigation math stays unit-testable.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bucket {
    ToReview,
    Viewed,
    Kind(diffler_core::classify::Kind),
}

impl Bucket {
    pub fn label(self) -> &'static str {
        match self {
            Self::ToReview => "To review",
            Self::Viewed => "Viewed",
            Self::Kind(kind) => kind.label(),
        }
    }
}

/// The trie flattening emits only `Dir` and `File`; the grouped layouts add the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNode {
    /// `path` is the fold key.
    Dir {
        path: String,
        name: String,
    },
    /// `index` points into the source path slice.
    File {
        index: usize,
        name: String,
    },
    Section {
        bucket: Bucket,
        count: usize,
        folded: bool,
    },
    /// Carries no title, since the renderer reads the session's walkthrough.
    Stop {
        index: usize,
    },
    WalkthroughSummary,
}

impl TreeNode {
    pub fn is_group(&self) -> bool {
        matches!(self, Self::Dir { .. } | Self::Section { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub node: TreeNode,
}

enum Entry {
    Dir(Node),
    File { index: usize, name: String },
}

/// Children keep insertion order; directories move ahead of files only at flatten time.
struct Node {
    path: String,
    name: String,
    children: Vec<Entry>,
}

impl Node {
    fn root() -> Self {
        Self {
            path: String::new(),
            name: String::new(),
            children: Vec::new(),
        }
    }

    /// Returns an index so the caller can re-borrow `children` to descend.
    fn dir_child_index(&mut self, name: &str) -> usize {
        if let Some(position) = self
            .children
            .iter()
            .position(|child| matches!(child, Entry::Dir(node) if node.name == name))
        {
            return position;
        }
        let path = if self.path.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.path)
        };
        self.children.push(Entry::Dir(Node {
            path,
            name: name.to_owned(),
            children: Vec::new(),
        }));
        self.children.len() - 1
    }
}

fn insert(root: &mut Node, path: &str, index: usize) {
    let mut node = root;
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            node.children.push(Entry::File {
                index,
                name: component.to_owned(),
            });
            return;
        }
        let child = node.dir_child_index(component);
        let Some(Entry::Dir(next)) = node.children.get_mut(child) else {
            return;
        };
        node = next;
    }
}

/// Joins a chain of lone subdirectories into one `a/b/c` row, neo-tree style;
/// the deepest node's path is the fold key.
fn collapse_chain(dir: &Node) -> (String, &Node) {
    let mut name = dir.name.clone();
    let mut node = dir;
    while node.children.len() == 1 {
        let Some(Entry::Dir(only)) = node.children.first() else {
            break;
        };
        name.push('/');
        name.push_str(&only.name);
        node = only;
    }
    (name, node)
}

fn flatten(
    node: &Node,
    depth: usize,
    folded: &BTreeSet<String>,
    promote: &dyn Fn(usize) -> bool,
    rows: &mut Vec<TreeRow>,
) {
    for child in &node.children {
        if let Entry::Dir(dir) = child {
            let (name, deepest) = collapse_chain(dir);
            rows.push(TreeRow {
                depth,
                node: TreeNode::Dir {
                    path: deepest.path.clone(),
                    name,
                },
            });
            if !folded.contains(&deepest.path) {
                flatten(deepest, depth + 1, folded, promote, rows);
            }
        }
    }
    let mut files: Vec<(usize, &String)> = node
        .children
        .iter()
        .filter_map(|child| match child {
            Entry::File { index, name } => Some((*index, name)),
            Entry::Dir(_) => None,
        })
        .collect();
    // a stable sort, so both groups keep the order the reader already learned
    files.sort_by_key(|&(index, _)| !promote(index));
    rows.extend(files.into_iter().map(|(index, name)| TreeRow {
        depth,
        node: TreeNode::File {
            index,
            name: name.clone(),
        },
    }));
}

/// Directories come before files at each level, and each kind keeps input order.
pub fn visible_rows(paths: &[&str], folded: &BTreeSet<String>) -> Vec<TreeRow> {
    visible_rows_promoting(paths, folded, &|_| false)
}

/// [`visible_rows`], with the files `promote` accepts listed first in each directory.
pub fn visible_rows_promoting(
    paths: &[&str],
    folded: &BTreeSet<String>,
    promote: &dyn Fn(usize) -> bool,
) -> Vec<TreeRow> {
    let mut root = Node::root();
    for (index, path) in paths.iter().enumerate() {
        insert(&mut root, path, index);
    }
    let mut rows = Vec::new();
    flatten(&root, 0, folded, promote, &mut rows);
    rows
}

/// A tree with no `Dir` rows, so one cursor logic drives both layouts.
pub fn flat_rows(paths: &[&str]) -> Vec<TreeRow> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| TreeRow {
            depth: 0,
            node: TreeNode::File {
                index,
                name: (*path).to_owned(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_folds() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn shape(row: &TreeRow) -> (usize, &'static str, String) {
        match &row.node {
            TreeNode::Dir { name, .. } => (row.depth, "dir", name.clone()),
            TreeNode::File { name, .. } => (row.depth, "file", name.clone()),
            TreeNode::Section { bucket, .. } => (row.depth, "section", bucket.label().to_owned()),
            TreeNode::Stop { index } => (row.depth, "stop", index.to_string()),
            TreeNode::WalkthroughSummary => (row.depth, "walkthrough_summary", String::new()),
        }
    }

    fn shapes(rows: &[TreeRow]) -> Vec<(usize, &'static str, String)> {
        rows.iter().map(shape).collect()
    }

    #[test]
    fn single_directory_chains_collapse_into_one_row() {
        let rows = visible_rows(&["a/b/c/d/file.rs"], &no_folds());
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "a/b/c/d".to_owned()),
                (1, "file", "file.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn a_chain_stops_collapsing_where_a_directory_branches() {
        let rows = visible_rows(&["top/mid/sub/x.rs", "top/mid/y.rs"], &no_folds());
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "top/mid".to_owned()),
                (1, "dir", "sub".to_owned()),
                (2, "file", "x.rs".to_owned()),
                (1, "file", "y.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn folding_a_collapsed_chain_hides_its_file_via_the_deepest_path() {
        let mut folded = no_folds();
        folded.insert("a/b/c/d".to_owned());
        let rows = visible_rows(&["a/b/c/d/file.rs"], &folded);
        assert_eq!(shapes(&rows), vec![(0, "dir", "a/b/c/d".to_owned())]);
    }

    #[test]
    fn nested_paths_produce_dir_then_file_rows_in_depth_order() {
        let rows = visible_rows(&["src/app/diff.rs", "src/lib.rs"], &no_folds());
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "src".to_owned()),
                (1, "dir", "app".to_owned()),
                (2, "file", "diff.rs".to_owned()),
                (1, "file", "lib.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn a_shared_directory_appears_once_for_many_files() {
        let rows = visible_rows(&["src/a.rs", "src/b.rs", "src/c.rs"], &no_folds());
        let dirs = rows
            .iter()
            .filter(|r| matches!(r.node, TreeNode::Dir { .. }))
            .count();
        assert_eq!(dirs, 1, "src/ collapses to a single dir row");
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "src".to_owned()),
                (1, "file", "a.rs".to_owned()),
                (1, "file", "b.rs".to_owned()),
                (1, "file", "c.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn dirs_sort_before_files_with_stable_order_within_a_kind() {
        let rows = visible_rows(&["z_root.rs", "pkg/inner.rs", "a_root.rs"], &no_folds());
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "pkg".to_owned()),
                (1, "file", "inner.rs".to_owned()),
                (0, "file", "z_root.rs".to_owned()),
                (0, "file", "a_root.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn promoted_files_lead_each_directory_without_disturbing_the_rest() {
        let paths = ["src/a.rs", "src/b.rs", "src/c.rs", "top.rs", "other.rs"];
        let rows = visible_rows_promoting(&paths, &no_folds(), &|index| index == 1 || index == 4);
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "src".to_owned()),
                (1, "file", "b.rs".to_owned()),
                (1, "file", "a.rs".to_owned()),
                (1, "file", "c.rs".to_owned()),
                (0, "file", "other.rs".to_owned()),
                (0, "file", "top.rs".to_owned()),
            ],
            "each directory promotes its own, and the others keep input order"
        );
    }

    #[test]
    fn promotion_leaves_directories_ahead_of_files() {
        let paths = ["top.rs", "src/a.rs"];
        let rows = visible_rows_promoting(&paths, &no_folds(), &|_| true);
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "src".to_owned()),
                (1, "file", "a.rs".to_owned()),
                (0, "file", "top.rs".to_owned()),
            ],
            "promotion sorts files among themselves, not past a directory"
        );
    }

    #[test]
    fn folding_a_dir_hides_its_subtree_but_keeps_its_row() {
        let mut folded = BTreeSet::new();
        folded.insert("src".to_owned());
        let rows = visible_rows(&["src/app/diff.rs", "src/lib.rs", "top.rs"], &folded);
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "src".to_owned()),
                (0, "file", "top.rs".to_owned()),
            ],
            "folded src/ shows its row only, its files and subdirs hidden"
        );
    }

    #[test]
    fn folding_an_inner_dir_hides_only_that_subtree() {
        let mut folded = BTreeSet::new();
        folded.insert("src/app".to_owned());
        let rows = visible_rows(&["src/app/diff.rs", "src/lib.rs"], &folded);
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "src".to_owned()),
                (1, "dir", "app".to_owned()),
                (1, "file", "lib.rs".to_owned()),
            ],
            "src/app is folded; src itself stays expanded"
        );
    }

    #[test]
    fn root_level_files_sit_at_depth_zero() {
        let rows = visible_rows(&["a.rs", "b.rs"], &no_folds());
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "file", "a.rs".to_owned()),
                (0, "file", "b.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn a_single_file_is_one_row() {
        let rows = visible_rows(&["only.rs"], &no_folds());
        assert_eq!(rows.len(), 1);
        assert_eq!(shape(&rows[0]), (0, "file", "only.rs".to_owned()));
    }

    #[test]
    fn empty_input_is_no_rows() {
        assert!(visible_rows(&[], &no_folds()).is_empty());
    }

    #[test]
    fn flat_rows_is_one_file_row_per_path_at_depth_zero() {
        let rows = flat_rows(&["src/lib.rs", "top.rs"]);
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "file", "src/lib.rs".to_owned()),
                (0, "file", "top.rs".to_owned()),
            ]
        );
    }

    #[test]
    fn identical_basenames_in_different_dirs_keep_their_own_indices() {
        let paths = ["a/mod.rs", "b/mod.rs"];
        let rows = visible_rows(&paths, &no_folds());
        let files: Vec<(usize, &str)> = rows
            .iter()
            .filter_map(|r| match &r.node {
                TreeNode::File { index, name } => Some((*index, name.as_str())),
                TreeNode::Dir { .. }
                | TreeNode::Section { .. }
                | TreeNode::Stop { .. }
                | TreeNode::WalkthroughSummary => None,
            })
            .collect();
        assert_eq!(files, vec![(0, "mod.rs"), (1, "mod.rs")]);
    }

    #[test]
    fn file_index_points_back_into_the_source_slice() {
        let paths = ["src/lib.rs", "top.rs", "src/app/diff.rs"];
        let rows = visible_rows(&paths, &no_folds());
        for row in &rows {
            if let TreeNode::File { index, name } = &row.node {
                let basename = paths[*index].rsplit('/').next().unwrap();
                assert_eq!(name, basename, "index {index} addresses its own path");
            }
        }
    }
}
