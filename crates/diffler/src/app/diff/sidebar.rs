//! The diff screen's file sidebar: its layouts, groups, rows and the declared-kinds worker.

use std::collections::{BTreeSet, HashMap, HashSet};

use diffler_core::classify::Kind;
use diffler_core::model::DiffModel;
use diffler_core::review::Review;
use diffler_core::session::Session;

use super::DiffView;
use super::slide::stop_file_index;
use crate::app::{App, Flow};
use crate::config::FileLayout;
use crate::tree::{self, Bucket, TreeNode, TreeRow};

/// Which section headers are collapsed, across both layouts that have them.
/// Viewed, Generated and Assets start folded, since nobody reads them by hand.
#[derive(Debug, Clone)]
pub(crate) struct BucketFolds(BTreeSet<Bucket>);

impl Default for BucketFolds {
    fn default() -> Self {
        Self(
            [
                Bucket::Viewed,
                Bucket::Kind(Kind::Generated),
                Bucket::Kind(Kind::Assets),
            ]
            .into(),
        )
    }
}

impl BucketFolds {
    pub(super) fn is_folded(&self, bucket: Bucket) -> bool {
        self.0.contains(&bucket)
    }

    pub(super) fn toggle_fold(&mut self, bucket: Bucket) {
        if !self.0.remove(&bucket) {
            self.0.insert(bucket);
        }
    }

    /// Open a section, reporting whether it had been folded.
    pub(super) fn unfold(&mut self, bucket: Bucket) -> bool {
        self.0.remove(&bucket)
    }
}

impl DiffView {
    /// The flattened sidebar rows over the model's files, each keeping its
    /// model index. Every group lists its viewed files first, so what is left
    /// to read is one run at its bottom.
    pub(crate) fn tree_rows(&self, model: &DiffModel, session: &Session) -> Vec<TreeRow> {
        match self.layout {
            FileLayout::Review => self.section_rows(model, Self::review_groups(model, session)),
            FileLayout::Kinds => self.section_rows(model, self.kind_groups(model, session)),
            FileLayout::Walkthrough => match self.active_walkthrough(session) {
                Some(walkthrough) => walkthrough
                    .summary
                    .is_some()
                    .then_some(TreeRow {
                        depth: 0,
                        node: TreeNode::WalkthroughSummary,
                    })
                    .into_iter()
                    .chain((0..walkthrough.stops.len()).map(|index| TreeRow {
                        depth: 0,
                        node: TreeNode::Stop { index },
                    }))
                    .collect(),
                None => {
                    tree::visible_rows_promoting(&Self::paths(model), &self.folded_dirs, &|index| {
                        Self::is_viewed(model, session, index)
                    })
                }
            },
            // list belongs to the status screen (config rejects it here); a
            // stray value degrades to the tree
            FileLayout::Tree | FileLayout::List => {
                tree::visible_rows_promoting(&Self::paths(model), &self.folded_dirs, &|index| {
                    Self::is_viewed(model, session, index)
                })
            }
        }
    }

    /// Every file the sidebar lists, in the order it lists them, as if nothing
    /// were folded. A reader walks the order on screen, so the commands that
    /// step files read it from here.
    pub(crate) fn display_order(&self, model: &DiffModel, session: &Session) -> Vec<usize> {
        match self.layout {
            FileLayout::Review => Self::review_groups(model, session),
            FileLayout::Kinds => self.kind_groups(model, session),
            FileLayout::Walkthrough if self.active_walkthrough(session).is_some() => {
                let stops = self
                    .active_walkthrough(session)
                    .into_iter()
                    .flat_map(|walkthrough| &walkthrough.stops);
                let mut seen = HashSet::new();
                return stops
                    .filter_map(|id| stop_file_index(model, session, id))
                    .filter(|index| seen.insert(*index))
                    .collect();
            }
            FileLayout::Walkthrough | FileLayout::Tree | FileLayout::List => {
                let rows =
                    tree::visible_rows_promoting(&Self::paths(model), &BTreeSet::new(), &|index| {
                        Self::is_viewed(model, session, index)
                    });
                return rows.iter().filter_map(row_file_index).collect();
            }
        }
        .into_iter()
        .flat_map(|(_, indices)| indices)
        .collect()
    }

    fn paths(model: &DiffModel) -> Vec<&str> {
        model.files.iter().map(|file| file.path.as_str()).collect()
    }

    fn is_viewed(model: &DiffModel, session: &Session, index: usize) -> bool {
        model
            .files
            .get(index)
            .is_some_and(|file| session.is_viewed(&file.path, &file.content_hash()))
    }

    /// Order a group's files for display: viewed first, each run keeping the
    /// order the diff gave it.
    fn viewed_first(model: &DiffModel, session: &Session, indices: &mut [usize]) {
        indices.sort_by_key(|&index| !Self::is_viewed(model, session, index));
    }

    /// A header row per group, its files beneath it unless it is folded. The
    /// renderer reads the depths to draw the indent, so both grouped layouts
    /// emit them from here.
    fn section_rows(
        &self,
        model: &DiffModel,
        groups: impl IntoIterator<Item = (Bucket, Vec<usize>)>,
    ) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for (bucket, indices) in groups {
            let folded = self.bucket_folds.is_folded(bucket);
            rows.push(TreeRow {
                depth: 0,
                node: TreeNode::Section {
                    bucket,
                    count: indices.len(),
                    folded,
                },
            });
            if folded {
                continue;
            }
            rows.extend(indices.into_iter().filter_map(|index| {
                let file = model.files.get(index)?;
                Some(TreeRow {
                    depth: 1,
                    node: TreeNode::File {
                        index,
                        name: file.path.clone(),
                    },
                })
            }));
        }
        rows
    }

    fn review_groups(model: &DiffModel, session: &Session) -> Vec<(Bucket, Vec<usize>)> {
        let (mut fresh, mut viewed) = (Vec::new(), Vec::new());
        for index in 0..model.files.len() {
            if Self::is_viewed(model, session, index) {
                viewed.push(index);
            } else {
                fresh.push(index);
            }
        }
        vec![(Bucket::ToReview, fresh), (Bucket::Viewed, viewed)]
    }

    /// One group per kind that the diff has files for, in [`Kind::ALL`] order.
    /// Files keep their model index, and their order within the diff behind
    /// the ones already viewed.
    fn kind_groups(&self, model: &DiffModel, session: &Session) -> Vec<(Bucket, Vec<usize>)> {
        // `Kind::ALL` below is the display order
        let mut grouped: HashMap<Kind, Vec<usize>> = HashMap::new();
        for (index, file) in model.files.iter().enumerate() {
            grouped
                .entry(self.kind_of(&file.path))
                .or_default()
                .push(index);
        }
        Kind::ALL
            .into_iter()
            .filter_map(|kind| {
                let mut indices = grouped.remove(&kind)?;
                Self::viewed_first(model, session, &mut indices);
                Some((Bucket::Kind(kind), indices))
            })
            .collect()
    }

    /// Model indices a sidebar bucket holds, folded or not. Derived from the
    /// same grouping the rows come from, so a group verb covers exactly what
    /// its header stands for.
    pub(crate) fn bucket_files(
        &self,
        model: &DiffModel,
        session: &Session,
        bucket: Bucket,
    ) -> Vec<usize> {
        let groups = match bucket {
            Bucket::Kind(_) => self.kind_groups(model, session),
            Bucket::ToReview | Bucket::Viewed => Self::review_groups(model, session),
        };
        groups
            .into_iter()
            .find(|(known, _)| *known == bucket)
            .map_or_else(Vec::new, |(_, indices)| indices)
    }

    pub(crate) fn kind_of(&self, path: &str) -> Kind {
        self.rules.kind(path, self.declared.get(path).copied())
    }

    /// The paths a declared-kinds worker should ask git about, empty unless
    /// the kinds layout is the one on screen.
    fn declared_targets(&self, review: &Review) -> Vec<String> {
        if self.layout != FileLayout::Kinds {
            return Vec::new();
        }
        self.model(review)
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect()
    }

    /// Advance the sidebar layout: tree, review, kinds, walkthrough. The
    /// walkthrough joins the cycle only when the review has one, and when the
    /// diff has no files of its own (`empty`) we stay on it, since the other
    /// layouts would list nothing.
    pub(crate) fn cycle_layout(&mut self, has_walkthrough: bool, empty: bool) -> FileLayout {
        self.layout = match self.layout {
            FileLayout::Review => FileLayout::Kinds,
            FileLayout::Kinds if has_walkthrough => FileLayout::Walkthrough,
            FileLayout::Walkthrough if empty && has_walkthrough => FileLayout::Walkthrough,
            FileLayout::Kinds | FileLayout::Walkthrough => FileLayout::Tree,
            FileLayout::Tree | FileLayout::List => FileLayout::Review,
        };
        self.layout
    }

    #[cfg(test)]
    pub(crate) fn sidebar_tree(&self, review: &Review) -> Vec<TreeRow> {
        sidebar_rows(self, review)
    }

    /// Seat the tree cursor on the selected file's row, or clamp it into
    /// range when that row is hidden (folded away, moved between buckets).
    pub(crate) fn reseat_tree_cursor(&mut self, rows: &[TreeRow]) {
        self.tree_cursor = tree_position_of_file(rows, self.selected)
            .unwrap_or_else(|| self.tree_cursor.min(rows.len().saturating_sub(1)));
    }
}

/// The sidebar rows for `diff`, pairing its model with its source's session
/// (the review layout needs the viewed marks).
pub(super) fn sidebar_rows(diff: &DiffView, review: &Review) -> Vec<TreeRow> {
    diff.tree_rows(diff.model(review), review.session_for(&diff.source))
}

/// Model index of the next file after the selection not yet marked viewed,
/// scanning forward and coming back around only when `wrap` asks for it.
pub(super) fn next_unviewed_index(diff: &DiffView, review: &Review, wrap: bool) -> Option<usize> {
    let model = diff.model(review);
    let session = review.session_for(&diff.source);
    // the order on screen: a jump has to land where the reader can see it came
    // from
    let order = diff.display_order(model, session);
    let count = order.len();
    let at = order.iter().position(|&index| index == diff.selected)?;
    (1..=count)
        .map(|step| at + step)
        .take_while(|&position| wrap || position < count)
        .filter_map(|position| order.get(position % count).copied())
        .find(|&index| !DiffView::is_viewed(model, session, index))
}

/// The model file a row addresses, for the walks that step files past the
/// directory and section headers between them.
pub(super) fn row_file_index(row: &TreeRow) -> Option<usize> {
    match row.node {
        TreeNode::File { index, .. } => Some(index),
        TreeNode::Dir { .. }
        | TreeNode::Section { .. }
        | TreeNode::Stop { .. }
        | TreeNode::WalkthroughSummary => None,
    }
}

/// Visible-row index of the File row addressing `file_index`, if shown.
pub(super) fn tree_position_of_file(rows: &[TreeRow], file_index: usize) -> Option<usize> {
    rows.iter()
        .position(|row| row_file_index(row) == Some(file_index))
}

/// Row position of the next thing the sidebar lists after `at`, `forward` down
/// the list or back up it, skipping the headers in between. A stop is what the
/// walkthrough layout lists, so `<c-n>` steps stops there and files everywhere
/// else.
pub(super) fn step_file_row(rows: &[TreeRow], at: usize, forward: bool) -> Option<usize> {
    crate::app::step_to(rows, at, forward, |row| {
        row_file_index(row).is_some() || matches!(row.node, TreeNode::Stop { .. })
    })
}

/// Paths whose git attributes a worker should read, for the kinds sidebar.
#[derive(Debug, Clone)]
pub struct DeclaredRequest {
    pub paths: Vec<String>,
    /// The request this answers; an answer for a file list the view has since
    /// replaced is dropped.
    pub token: u64,
}

impl App {
    /// Ask git what the repo declares about the kinds sidebar's files. The
    /// lookup walks the attribute files per path, so it runs on a worker and
    /// the sidebar groups by the built-in table until the answer lands.
    pub(crate) fn queue_declared(&mut self) {
        let paths = self
            .diff
            .as_ref()
            .map(|diff| diff.declared_targets(&self.review))
            .unwrap_or_default();
        self.declared_token += 1;
        let token = self.declared_token;
        self.pending_declared = (!paths.is_empty()).then_some(DeclaredRequest { paths, token });
    }

    pub(crate) fn on_declared_kinds(&mut self, kinds: HashMap<String, Kind>, token: u64) -> Flow {
        let Some(diff) = self.diff.as_mut().filter(|_| token == self.declared_token) else {
            return Flow::Idle;
        };
        if diff.declared == kinds {
            return Flow::Idle;
        }
        diff.declared = kinds;
        diff.invalidate();
        diff.ensure_rows(&self.review);
        Flow::Continue
    }
}
