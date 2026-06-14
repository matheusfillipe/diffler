//! Pure directory-trie flattening shared by the diff sidebar and the status
//! sections. A file list (repo-relative paths, in input order) becomes a list
//! of visible rows honoring fold state — directories before files at each
//! level, input order preserved within a kind, folded directories hiding their
//! subtree. No rendering and no app state, so the navigation math is fully
//! unit-testable.

use std::collections::BTreeSet;

/// One node in a flattened tree row: a directory (carrying its full path as the
/// fold key, and the last path segment as its display name) or a file (carrying
/// its index into the source path slice, and its basename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNode {
    Dir { path: String, name: String },
    File { index: usize, name: String },
}

/// A flattened tree row: a node and its indentation depth (0 at the root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub node: TreeNode,
}

/// An entry under a directory while the trie is built, before flattening.
enum Entry {
    Dir(Node),
    File { index: usize, name: String },
}

/// A directory's children, kept in insertion order so input order survives
/// within a kind; directories are pulled ahead of files only at flatten time.
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

    /// Find the index of the child directory named `name`, creating it if
    /// absent. New directories carry the full path so they can serve as fold
    /// keys. Returning the index (not a reference) lets the caller re-borrow
    /// `children` to descend, keeping the build panic-free.
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

/// Insert one file path (its components) under `root`, recording the index it
/// occupies in the source slice on the leaf.
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

/// Append a node's children to `rows`, directories first then files, recursing
/// into expanded directories. A folded directory contributes its own row but
/// none of its descendants.
/// Collapse a chain of single-directory children into one row, neo-tree style:
/// `a/b/c` where each level holds exactly one subdirectory becomes a single
/// `a/b/c` node. Returns the joined display name and the deepest node (whose
/// path is the fold key and whose children are rendered beneath the row). The
/// chain stops at the first directory that holds a file or more than one child.
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

fn flatten(node: &Node, depth: usize, folded: &BTreeSet<String>, rows: &mut Vec<TreeRow>) {
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
                flatten(deepest, depth + 1, folded, rows);
            }
        }
    }
    for child in &node.children {
        if let Entry::File { index, name } = child {
            rows.push(TreeRow {
                depth,
                node: TreeNode::File {
                    index: *index,
                    name: name.clone(),
                },
            });
        }
    }
}

/// Build the flattened, fold-respecting visible rows for `paths` (each a
/// file's repo-relative path, in input order). `folded` holds folded directory
/// paths. Directories are sorted before files at each level; entries keep input
/// order within a kind. A directory not in `folded` is expanded.
pub fn visible_rows(paths: &[&str], folded: &BTreeSet<String>) -> Vec<TreeRow> {
    let mut root = Node::root();
    for (index, path) in paths.iter().enumerate() {
        insert(&mut root, path, index);
    }
    let mut rows = Vec::new();
    flatten(&root, 0, folded, &mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_folds() -> BTreeSet<String> {
        BTreeSet::new()
    }

    /// Compact `(depth, kind, name)` view of a row for terse assertions.
    fn shape(row: &TreeRow) -> (usize, &'static str, String) {
        match &row.node {
            TreeNode::Dir { name, .. } => (row.depth, "dir", name.clone()),
            TreeNode::File { name, .. } => (row.depth, "file", name.clone()),
        }
    }

    fn shapes(rows: &[TreeRow]) -> Vec<(usize, &'static str, String)> {
        rows.iter().map(shape).collect()
    }

    #[test]
    fn single_directory_chains_collapse_into_one_row() {
        // a/b/c/d each hold exactly one subdirectory → one joined row
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
        // top/ holds one dir (mid/) → collapses to top/mid; mid/ branches
        // (a dir and a file) so it stops there
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
        // input order: a root file, then a dir's file, then another root file
        let rows = visible_rows(&["z_root.rs", "pkg/inner.rs", "a_root.rs"], &no_folds());
        assert_eq!(
            shapes(&rows),
            vec![
                (0, "dir", "pkg".to_owned()),
                (1, "file", "inner.rs".to_owned()),
                // root files keep their input order, after the dir
                (0, "file", "z_root.rs".to_owned()),
                (0, "file", "a_root.rs".to_owned()),
            ]
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
    fn identical_basenames_in_different_dirs_keep_their_own_indices() {
        let paths = ["a/mod.rs", "b/mod.rs"];
        let rows = visible_rows(&paths, &no_folds());
        let files: Vec<(usize, &str)> = rows
            .iter()
            .filter_map(|r| match &r.node {
                TreeNode::File { index, name } => Some((*index, name.as_str())),
                TreeNode::Dir { .. } => None,
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
