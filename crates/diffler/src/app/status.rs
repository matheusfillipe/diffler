//! Status screen state and handlers: neogit-style sections with inline
//! diff expansion, folding, stage/unstage/discard, and cursor preservation
//! across refreshes.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use diffler_core::model::FileDiff;
use diffler_core::vcs::{BranchInfo, HeadInfo, LogEntry, NetworkOp, Vcs, VcsError};

use super::enrich::EnrichOutcome;
use super::{App, BranchAction, FileHighlights, Modal, PendingOp};
use crate::config::FileLayout;
use crate::keymap::Action;
use crate::tree::{self, TreeNode, TreeRow};

/// Heading for the trailing unpushed-commits section, shared by the renderer
/// and the search labels so a `/` match lines up with the displayed text.
pub(crate) const UNPUSHED_TITLE: &str = "Unpushed";

/// Heading for the trailing recent-commits section, shared by the renderer and
/// the search labels so a `/` match lines up with the displayed text.
pub(crate) const RECENT_TITLE: &str = "Recent commits";

/// Heading for the trailing CI-runs section (when a provider is detected).
pub(crate) const CI_TITLE: &str = "CI runs";

/// Heading for the trailing Branches section.
pub(crate) const BRANCHES_TITLE: &str = "Branches";

/// How many recent runs the inline status section shows (the full list lives on
/// the Runs screen).
const CI_INLINE_LIMIT: usize = 5;

/// How many local branches the inline status section shows (the full list is
/// reachable through the branch picker).
const BRANCHES_INLINE_LIMIT: usize = 10;

/// Status screen sections, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Untracked,
    Unstaged,
    Staged,
}

impl Section {
    pub const ALL: [Self; 3] = [Self::Untracked, Self::Unstaged, Self::Staged];

    pub fn title(self) -> &'static str {
        match self {
            Self::Untracked => "Untracked",
            Self::Unstaged => "Unstaged changes",
            Self::Staged => "Staged changes",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Untracked => 0,
            Self::Unstaged => 1,
            Self::Staged => 2,
        }
    }
}

/// One cursor-addressable row of the status screen: section headers, directory
/// rows, file rows, and (when a file is expanded inline) hunk headers and
/// diff lines, plus the trailing Recent commits section. Holds an owned
/// directory path (the fold key), so it is `Clone`, not `Copy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    SectionHeader {
        section: Section,
        count: usize,
    },
    /// A directory in a section's file tree; `path` is the fold key, `name` the
    /// display name (a joined `a/b/c` for a collapsed single-child chain),
    /// `depth` the indentation under the header.
    Dir {
        section: Section,
        path: String,
        name: String,
        depth: usize,
    },
    File {
        section: Section,
        index: usize,
        depth: usize,
    },
    HunkHeader {
        section: Section,
        file: usize,
        hunk: usize,
    },
    DiffLine {
        section: Section,
        file: usize,
        hunk: usize,
        line: usize,
    },
    /// Header of the trailing Unpushed section: commits on HEAD its
    /// upstream doesn't have yet.
    UnpushedHeader {
        count: usize,
    },
    /// One such commit; `index` into `StatusView::unpushed`.
    Unpushed {
        index: usize,
    },
    RecentHeader {
        count: usize,
    },
    Commit {
        index: usize,
    },
    /// The branch's open pull request; Enter reviews it.
    Pr,
    /// Divider between the branch band (this branch) and the repo band (this
    /// repo). Furniture, not a cursor-addressable row: it never holds the
    /// cursor, has no search label, and is skipped by every movement.
    RepoDivider,
    /// Header of the trailing Branches section.
    BranchesHeader {
        count: usize,
    },
    /// One local branch in the inline section; `index` into
    /// `StatusView::branches`.
    Branch {
        index: usize,
    },
    /// Header of the trailing CI-runs section.
    CiHeader {
        count: usize,
    },
    /// One CI run in the inline section; `index` into `App::runs`.
    CiRun {
        index: usize,
    },
}

/// Where the cursor logically sits, so it can be restored after a refresh
/// reshuffles the rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CursorAnchor {
    Section(Section),
    Dir {
        section: Section,
        path: String,
    },
    File {
        section: Section,
        path: String,
        hunk: Option<usize>,
    },
    Unpushed,
    UnpushedCommit(usize),
    Recent,
    Commit(usize),
    Pr,
    Branches,
    BranchRow(usize),
    Ci,
    CiRun(usize),
}

/// All state owned by the status screen.
#[allow(clippy::struct_excessive_bools)] // independent per-section fold flags, not a state machine
pub struct StatusView {
    pub cursor: usize,
    pub folded: [bool; 3],
    /// Commits on HEAD its upstream lacks: local, free to act on, so shown
    /// unfolded by default.
    pub unpushed: Vec<LogEntry>,
    pub unpushed_folded: bool,
    pub recent: Vec<LogEntry>,
    pub recent_folded: bool,
    /// Whether the trailing CI-runs section is collapsed.
    pub ci_folded: bool,
    /// Local branches, newest tip first, capped to `BRANCHES_INLINE_LIMIT`.
    pub branches: Vec<BranchInfo>,
    /// Whether the trailing Branches section is collapsed; the whole repo
    /// band starts folded, unlike the always-open branch band above it.
    pub branches_folded: bool,
    /// Body height of the last render, so half-page motions step by a screenful.
    pub(crate) viewport: u16,
    /// Per-section set of file paths whose inline diff is expanded.
    expanded: [BTreeSet<String>; 3],
    /// Per-section set of folded directory paths in that section's file tree.
    folded_dirs: [BTreeSet<String>; 3],
    /// Per-section set of file paths whose inline diff has received its
    /// background enrichment (intra-line emphasis), so a landed file isn't
    /// re-queued. Cleared when the status sections are rebuilt (refresh).
    enriched: [BTreeSet<String>; 3],
    /// Per-file syntax spans for inline diffs, keyed by path plus both-sides
    /// content hash: a partially staged file shows up in two sections with
    /// different content, and each needs its own entry to stay settled.
    /// Filled lazily, only for expanded files.
    pub(crate) highlights: HashMap<(String, String), FileHighlights>,
    /// Last render's body rect, line scroll, and per-rendered-line row index
    /// (rows vary in height, so a screen row maps back to a `visible_rows`
    /// index only through this table). Drives mouse hit-testing.
    pub(crate) body: ratatui::layout::Rect,
    pub(crate) scroll: u16,
    pub(crate) line_rows: Vec<Option<usize>>,
}

impl StatusView {
    pub(super) fn new(
        unpushed: Vec<LogEntry>,
        recent: Vec<LogEntry>,
        branches: Vec<BranchInfo>,
    ) -> Self {
        Self {
            cursor: 0,
            folded: [false; 3],
            unpushed,
            unpushed_folded: false,
            recent,
            recent_folded: true,
            ci_folded: true,
            branches,
            branches_folded: true,
            viewport: 0,
            expanded: [const { BTreeSet::new() }; 3],
            folded_dirs: [const { BTreeSet::new() }; 3],
            enriched: [const { BTreeSet::new() }; 3],
            highlights: HashMap::new(),
            body: ratatui::layout::Rect::default(),
            scroll: 0,
            line_rows: Vec::new(),
        }
    }

    /// Forget which inline diffs have been enriched (after a refresh rebuilds
    /// the status sections unenriched).
    pub(super) fn clear_enriched(&mut self) {
        for set in &mut self.enriched {
            set.clear();
        }
    }
}

/// The Unpushed and Recent commit lists, as `(unpushed, recent)`. Loaded
/// together and returned or discarded as a pair: recent is filtered against
/// unpushed, so applying one without the other leaves a pushed commit missing
/// from both sections. Recent is walked deep enough to still fill `limit`
/// after the unpushed commits are taken out of it.
pub(super) fn load_commit_lists(
    vcs: &dyn Vcs,
    head: &HeadInfo,
    limit: usize,
) -> Result<(Vec<LogEntry>, Vec<LogEntry>), VcsError> {
    let unpushed = match head.upstream.as_deref().filter(|_| head.ahead > 0) {
        Some(upstream) => vcs.commits_between(upstream, "HEAD")?,
        None => Vec::new(),
    };
    let mut recent = vcs.log(limit + unpushed.len())?;
    recent.retain(|entry| !unpushed.iter().any(|ahead| ahead.oid == entry.oid));
    recent.truncate(limit);
    Ok((unpushed, recent))
}

/// Every local branch, the checked-out one first and the rest newest tip first.
/// The inline section renders only the first `BRANCHES_INLINE_LIMIT`, so the
/// header still counts the whole repo and a capped list reads as a cut rather
/// than as the total. Head leads so sitting on an old branch while ten newer
/// ones exist cannot cut your own branch out of the list.
pub(super) fn load_branches(vcs: &dyn Vcs) -> Result<Vec<BranchInfo>, VcsError> {
    let mut branches = vcs.branches()?;
    branches.sort_by_key(|branch| (!branch.is_head, std::cmp::Reverse(branch.tip_unix)));
    Ok(branches)
}

impl App {
    pub fn is_folded(&self, section: Section) -> bool {
        self.status
            .folded
            .get(section.index())
            .copied()
            .unwrap_or(false)
    }

    pub fn is_expanded(&self, section: Section, path: &str) -> bool {
        self.status
            .expanded
            .get(section.index())
            .is_some_and(|set| set.contains(path))
    }

    /// Whether the directory `path` is folded in `section`'s file tree.
    pub fn is_dir_folded(&self, section: Section, path: &str) -> bool {
        self.status
            .folded_dirs
            .get(section.index())
            .is_some_and(|set| set.contains(path))
    }

    fn section_folded_dirs(&self, section: Section) -> BTreeSet<String> {
        self.status
            .folded_dirs
            .get(section.index())
            .cloned()
            .unwrap_or_default()
    }

    /// Layout-aware flattened rows for a section's files. The flat list is a
    /// degenerate tree (one File node per file, depth 0, no Dir nodes), so the
    /// caller's row-building and the cursor model are identical for both
    /// layouts. The tree honors the section's folded directories.
    fn section_layout_rows(&self, section: Section, files: &[FileDiff]) -> Vec<TreeRow> {
        match self.config.ui.status_file_layout {
            // review is diff-sidebar-only (config rejects it here); a stray
            // value degrades to the flat list
            FileLayout::List | FileLayout::Review => {
                let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
                tree::flat_rows(&paths)
            }
            FileLayout::Tree => {
                let folded = self.section_folded_dirs(section);
                let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
                tree::visible_rows(&paths, &folded)
            }
        }
    }

    /// Flattened cursor-addressable rows given current fold/expansion state.
    /// Empty sections are hidden, neogit-style; blank separators are a
    /// rendering concern, so j/k skip them by construction.
    pub fn visible_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for section in Section::ALL {
            let files = self.section_files(section);
            if files.is_empty() {
                continue;
            }
            rows.push(Row::SectionHeader {
                section,
                count: files.len(),
            });
            if self.is_folded(section) {
                continue;
            }
            // List renders a degenerate tree (one File row per file at depth 0,
            // no Dir rows), so the same cursor/navigation model serves both
            // layouts; Tree groups files under collapsible directory rows.
            for tree_row in self.section_layout_rows(section, files) {
                match tree_row.node {
                    // status sections never produce review buckets
                    TreeNode::Section { .. } => {}
                    TreeNode::Dir { path, name } => rows.push(Row::Dir {
                        section,
                        path,
                        name,
                        depth: tree_row.depth,
                    }),
                    TreeNode::File { index, .. } => {
                        rows.push(Row::File {
                            section,
                            index,
                            depth: tree_row.depth,
                        });
                        let Some(file) = files.get(index) else {
                            continue;
                        };
                        if !self.is_expanded(section, &file.path) {
                            continue;
                        }
                        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
                            rows.push(Row::HunkHeader {
                                section,
                                file: index,
                                hunk: hunk_index,
                            });
                            rows.extend((0..hunk.lines.len()).map(|line| Row::DiffLine {
                                section,
                                file: index,
                                hunk: hunk_index,
                                line,
                            }));
                        }
                    }
                }
            }
        }
        // this-branch band: unpushed commits, then the branch's own PR
        if !self.status.unpushed.is_empty() {
            rows.push(Row::UnpushedHeader {
                count: self.status.unpushed.len(),
            });
            if !self.status.unpushed_folded {
                rows.extend((0..self.status.unpushed.len()).map(|index| Row::Unpushed { index }));
            }
        }
        // the branch's own PR belongs beside its commits, and lands from an async
        // fetch, so whoever sets `pr` re-seats the cursor over the rows it displaces
        if self.pr.is_some() {
            rows.push(Row::Pr);
        }
        // this-repo band, folded by default: branches, recent commits, CI runs
        let has_repo_band = !self.status.branches.is_empty()
            || !self.status.recent.is_empty()
            || !self.runs.is_empty();
        if has_repo_band {
            rows.push(Row::RepoDivider);
        }
        if !self.status.branches.is_empty() {
            rows.push(Row::BranchesHeader {
                count: self.status.branches.len(),
            });
            if !self.status.branches_folded {
                let shown = self.status.branches.len().min(BRANCHES_INLINE_LIMIT);
                rows.extend((0..shown).map(|index| Row::Branch { index }));
            }
        }
        if !self.status.recent.is_empty() {
            rows.push(Row::RecentHeader {
                count: self.status.recent.len(),
            });
            if !self.status.recent_folded {
                rows.extend((0..self.status.recent.len()).map(|index| Row::Commit { index }));
            }
        }
        if !self.runs.is_empty() {
            rows.push(Row::CiHeader {
                count: self.runs.len(),
            });
            if !self.status.ci_folded {
                let shown = self.runs.len().min(CI_INLINE_LIMIT);
                rows.extend((0..shown).map(|index| Row::CiRun { index }));
            }
        }
        rows
    }

    /// Searchable `(row index, text)` pairs for the `/` search: section
    /// titles, directory names, file paths, and recent-commit lines. Inline
    /// diff rows are left out: the diff view is where code is searched.
    pub(crate) fn status_search_rows(&self) -> Vec<(usize, String)> {
        self.visible_rows()
            .iter()
            .enumerate()
            .filter_map(|(index, row)| self.status_row_label(row).map(|text| (index, text)))
            .collect()
    }

    fn status_row_label(&self, row: &Row) -> Option<String> {
        Some(match row {
            Row::SectionHeader { section, .. } => section.title().to_owned(),
            Row::UnpushedHeader { .. } => UNPUSHED_TITLE.to_owned(),
            Row::Unpushed { index } => self.status.unpushed.get(*index)?.subject.clone(),
            Row::RecentHeader { .. } => RECENT_TITLE.to_owned(),
            Row::Dir { name, .. } => name.clone(),
            Row::File { section, index, .. } => self
                .status_file_name(self.section_files(*section).get(*index)?)
                .to_owned(),
            Row::Commit { index } => self.status.recent.get(*index)?.subject.clone(),
            Row::BranchesHeader { .. } => BRANCHES_TITLE.to_owned(),
            Row::Branch { index } => self.status.branches.get(*index)?.name.clone(),
            Row::CiHeader { .. } => CI_TITLE.to_owned(),
            Row::Pr => {
                let pr = self.pr.as_ref()?;
                format!("PR #{} {}", pr.number, pr.title)
            }
            Row::CiRun { index } => self.runs.get(*index)?.name.clone(),
            Row::HunkHeader { .. } | Row::DiffLine { .. } | Row::RepoDivider => return None,
        })
    }

    /// The text a file row displays: the basename in the tree layout (the
    /// directory rows above carry the path), the whole repo-relative path in
    /// the flat list. The search labels and the renderer share it so a `/`
    /// match highlights exactly the displayed substring.
    pub(crate) fn status_file_name<'a>(&self, file: &'a FileDiff) -> &'a str {
        if self.config.ui.status_file_layout == FileLayout::List {
            file.path.as_str()
        } else {
            file.path.rsplit('/').next().unwrap_or(&file.path)
        }
    }

    pub fn section_files(&self, section: Section) -> &[FileDiff] {
        let model = match section {
            Section::Untracked => &self.review.status.untracked,
            Section::Unstaged => &self.review.status.unstaged,
            Section::Staged => &self.review.status.staged,
        };
        &model.files
    }

    /// Queue background enrichment (intra-line emphasis + syntax highlight)
    /// for every currently-expanded inline diff. Cheap and deduped by
    /// content, so the renderer calls it per frame; the expanded rows draw
    /// plain until the outcome lands as an `AppEvent::Enriched` event: draw
    /// only renders.
    pub(crate) fn queue_enrich_status_expanded(&mut self) {
        let semantic = self.config.ui.semantic_diff;
        for section in Section::ALL {
            let index = section.index();
            let model = match section {
                Section::Untracked => &self.review.status.untracked,
                Section::Unstaged => &self.review.status.unstaged,
                Section::Staged => &self.review.status.staged,
            };
            let (Some(expanded), Some(enriched)) = (
                self.status.expanded.get(index),
                self.status.enriched.get(index),
            ) else {
                continue;
            };
            for file in &model.files {
                if file.binary || file.hunks.is_empty() || !expanded.contains(&file.path) {
                    continue;
                }
                let ready = enriched.contains(&file.path)
                    && self
                        .status
                        .highlights
                        .contains_key(&(file.path.clone(), file.sides_hash()));
                super::enrich::queue_if_stale(
                    &mut self.enrich_inflight,
                    &mut self.pending_enrich,
                    file,
                    semantic,
                    ready,
                );
            }
        }
    }

    /// Install a finished enrichment into every status-section file it still
    /// matches (same path and content): swap in the emphasised hunks, cache
    /// the syntax highlights, and mark the file enriched so it isn't
    /// re-queued. A stale outcome (the file changed while the job ran)
    /// matches nothing and is dropped; the next frame re-queues.
    pub(super) fn install_status_enrichment(&mut self, outcome: &EnrichOutcome) {
        // entries for content no section still shows are dead; drop them so
        // an edited file doesn't accumulate one entry per past hash
        let live: Vec<String> = [
            &self.review.status.untracked,
            &self.review.status.unstaged,
            &self.review.status.staged,
        ]
        .into_iter()
        .flat_map(|model| &model.files)
        .filter(|file| file.path == outcome.path)
        .map(FileDiff::sides_hash)
        .collect();
        self.status
            .highlights
            .retain(|(path, hash), _| *path != outcome.path || live.contains(hash));
        for section in Section::ALL {
            let index = section.index();
            let model = match section {
                Section::Untracked => &mut self.review.status.untracked,
                Section::Unstaged => &mut self.review.status.unstaged,
                Section::Staged => &mut self.review.status.staged,
            };
            let Some(enriched) = self.status.enriched.get_mut(index) else {
                continue;
            };
            for file in &mut model.files {
                if file.path == outcome.path && file.sides_hash() == outcome.hash {
                    file.hunks.clone_from(&outcome.hunks);
                    self.status.highlights.insert(
                        (outcome.path.clone(), outcome.hash.clone()),
                        outcome.highlights.clone(),
                    );
                    enriched.insert(file.path.clone());
                }
            }
        }
    }

    /// Move the cursor by half a screenful, clamped to the visible rows.
    fn status_page(&mut self, down: bool, full: bool) {
        let step = super::page_step(self.status.viewport, full);
        let rows = self.visible_rows();
        let last = rows.len().saturating_sub(1);
        self.status.cursor = if down {
            nearest_selectable(&rows, (self.status.cursor + step).min(last), true)
        } else {
            nearest_selectable(&rows, self.status.cursor.saturating_sub(step), false)
        };
    }

    pub(super) fn status_mouse(&mut self, gesture: super::MouseGesture) {
        use super::MouseGesture;
        match gesture {
            MouseGesture::Scroll { down, .. } => {
                let delta = if down { 3 } else { -3 };
                let rows = self.visible_rows();
                let last = rows.len().saturating_sub(1);
                let target = self.status.cursor.saturating_add_signed(delta).min(last);
                self.status.cursor = nearest_selectable(&rows, target, down);
            }
            // single-click selects; double-click activates (open file/commit,
            // or fold the section/dir/recent header), like `<cr>`/`<tab>`
            MouseGesture::Press { col, row } => {
                self.status_select_at(col, row);
            }
            MouseGesture::DoublePress { col, row } => {
                if self.status_select_at(col, row) {
                    self.status_activate_cursor();
                }
            }
            // the status screen has no line selection to drag or cancel
            MouseGesture::Drag { .. } | MouseGesture::Cancel => {}
        }
    }

    /// Move the cursor to the row under `(col, row)`. Returns whether a row was
    /// hit (so a double-click only activates on a real row).
    fn status_select_at(&mut self, col: u16, row: u16) -> bool {
        let Some(line) = super::hit_index(self.status.body, self.status.scroll as usize, col, row)
        else {
            return false;
        };
        let Some(Some(index)) = self.status.line_rows.get(line).copied() else {
            return false;
        };
        if index >= self.visible_rows().len() {
            return false;
        }
        self.status.cursor = index;
        true
    }

    fn status_activate_cursor(&mut self) {
        match self.cursor_row() {
            Some(
                Row::File { .. }
                | Row::Unpushed { .. }
                | Row::Commit { .. }
                | Row::Branch { .. }
                | Row::CiRun { .. },
            ) => {
                self.open_at_cursor();
            }
            Some(
                Row::SectionHeader { .. }
                | Row::Dir { .. }
                | Row::UnpushedHeader { .. }
                | Row::BranchesHeader { .. }
                | Row::RecentHeader { .. }
                | Row::CiHeader { .. },
            ) => {
                self.toggle_fold();
            }
            _ => {}
        }
    }

    pub(super) fn dispatch_status(&mut self, action: Action) {
        match action {
            Action::MoveDown => {
                let rows = self.visible_rows();
                let last = rows.len().saturating_sub(1);
                self.status.cursor =
                    nearest_selectable(&rows, (self.status.cursor + 1).min(last), true);
            }
            Action::MoveUp => {
                let rows = self.visible_rows();
                self.status.cursor =
                    nearest_selectable(&rows, self.status.cursor.saturating_sub(1), false);
            }
            Action::HalfPageDown => self.status_page(true, false),
            Action::HalfPageUp => self.status_page(false, false),
            Action::FullPageDown => self.status_page(true, true),
            Action::FullPageUp => self.status_page(false, true),
            Action::GoTop => {
                let rows = self.visible_rows();
                self.status.cursor = nearest_selectable(&rows, 0, true);
            }
            Action::GoBottom => {
                let rows = self.visible_rows();
                let last = rows.len().saturating_sub(1);
                self.status.cursor = nearest_selectable(&rows, last, false);
            }
            Action::NextHunk => self.jump(true, is_hunk_header),
            Action::PrevHunk => self.jump(false, is_hunk_header),
            Action::NextSection => self.jump(true, is_section_header),
            Action::PrevSection => self.jump(false, is_section_header),
            Action::ToggleFold => self.toggle_fold(),
            Action::Stage => self.stage_at_cursor(),
            Action::Unstage => self.unstage_at_cursor(),
            Action::StageAll => self.stage_all(),
            Action::UnstageAll => self.unstage_all(),
            Action::Discard => self.discard_at_cursor(),
            Action::Open => self.open_at_cursor(),
            Action::OpenReviewDiff | Action::DiffWorkingTree => self.open_working_tree_diff(None),
            Action::DiffBase => self.diff_against_base(),
            Action::DiffLastCommit => self.open_against_diff("HEAD~1"),
            Action::DiffBranch => self.diff_against_branch(),
            Action::DiffCommit => self.diff_against_commit(),
            Action::MarkViewed => self.toggle_viewed(),
            Action::LogView => self.open_log(),
            Action::CommitFlow => self.commit_flow(),
            Action::CommitExtend => self.commit_extend(),
            Action::CommitAmend => self.commit_amend(),
            Action::CommitReword => self.commit_reword(),
            Action::BranchCheckout => self.open_branch_list(BranchAction::Checkout),
            Action::BranchCreateCheckout => self.branch_name_input(true),
            Action::BranchCreate => self.branch_name_input(false),
            Action::BranchDelete => self.open_branch_list(BranchAction::Delete),
            Action::Push => self.push(),
            Action::PushSetUpstream => self.push_set_upstream(),
            Action::Pull => self.pull(),
            Action::Fetch => self.request_network(NetworkOp::Fetch, "fetch"),
            Action::FetchAll => self.request_network(NetworkOp::FetchAll, "fetch --all"),
            Action::StashPush => self.stash_push(),
            Action::StashPop => self.stash_pop(),
            Action::OpenEditor => self.editor_at_status_cursor(),
            other => {
                self.info(format!("{} is not implemented yet", other.name()));
            }
        }
    }

    /// Review against the branch a pull request would target: the primary
    /// remote's default branch, or a local main/master.
    fn diff_against_base(&mut self) {
        let remotes = self.review.vcs.remotes().unwrap_or_default();
        let primary = remotes
            .iter()
            .find(|name| *name == "origin")
            .or_else(|| remotes.first())
            .map_or("origin", String::as_str);
        match self.review.vcs.default_branch(primary) {
            Ok(Some(branch)) => self.open_against_diff(&branch),
            Ok(None) => self.info("no base branch detected; pick one with d b"),
            Err(err) => self.error(err.to_string()),
        }
    }

    fn diff_against_branch(&mut self) {
        match self.review.vcs.all_branches() {
            Ok(branches) => {
                let entries = branches
                    .into_iter()
                    .map(|name| super::RevChoice {
                        rev: name.clone(),
                        label: name,
                    })
                    .collect();
                self.open_rev_list("Diff against branch", entries);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    fn diff_against_commit(&mut self) {
        match self.review.vcs.log(super::log::LOG_LIMIT) {
            Ok(entries) => {
                let entries = entries
                    .into_iter()
                    .map(|entry| super::RevChoice {
                        label: format!("{} {}", entry.oid7, entry.subject),
                        rev: entry.oid,
                    })
                    .collect();
                self.open_rev_list("Diff against commit", entries);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    fn editor_at_status_cursor(&mut self) {
        let Some(row) = self.cursor_row() else {
            self.info("no file under the cursor");
            return;
        };
        // For an expanded inline diff line, pass the line number so the
        // editor opens at the right spot, same as the dedicated diff view.
        let line_no = if let Row::DiffLine {
            section,
            file,
            hunk,
            line,
        } = row
        {
            self.section_files(section)
                .get(file)
                .and_then(|f| f.hunks.get(hunk))
                .and_then(|h| h.lines.get(line))
                .and_then(|l| l.new_no.or(l.old_no))
        } else {
            None
        };
        let Some(path) = self.row_file(&row).map(|(_, file, _)| file.path.clone()) else {
            self.info("no file under the cursor");
            return;
        };
        self.request_editor(&path, line_no);
    }

    fn cursor_row(&self) -> Option<Row> {
        self.visible_rows().get(self.status.cursor).cloned()
    }

    /// The file a row addresses, with the hunk index for hunk-scoped rows.
    pub fn row_file(&self, row: &Row) -> Option<(Section, &FileDiff, Option<usize>)> {
        match *row {
            Row::File { section, index, .. } => self
                .section_files(section)
                .get(index)
                .map(|file| (section, file, None)),
            Row::HunkHeader {
                section,
                file,
                hunk,
            }
            | Row::DiffLine {
                section,
                file,
                hunk,
                ..
            } => self
                .section_files(section)
                .get(file)
                .map(|f| (section, f, Some(hunk))),
            Row::Dir { .. }
            | Row::SectionHeader { .. }
            | Row::UnpushedHeader { .. }
            | Row::Unpushed { .. }
            | Row::Pr
            | Row::RepoDivider
            | Row::BranchesHeader { .. }
            | Row::Branch { .. }
            | Row::RecentHeader { .. }
            | Row::Commit { .. }
            | Row::CiHeader { .. }
            | Row::CiRun { .. } => None,
        }
    }

    fn stage_at_cursor(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        let Some((section, file, hunk)) = self.row_file(&row) else {
            return;
        };
        if section == Section::Staged {
            self.info("already staged");
            return;
        }
        let path = file.path.clone();
        match hunk {
            None => self.vcs_op(move |vcs| vcs.stage(Path::new(&path))),
            Some(hunk) => {
                let Some(id) = file.hunks.get(hunk).map(|h| h.id.clone()) else {
                    return;
                };
                self.vcs_op(move |vcs| vcs.stage_hunk(Path::new(&path), &id));
            }
        }
    }

    fn unstage_at_cursor(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        let Some((section, file, hunk)) = self.row_file(&row) else {
            return;
        };
        if section != Section::Staged {
            self.info("not staged");
            return;
        }
        let path = file.path.clone();
        match hunk {
            None => self.vcs_op(move |vcs| vcs.unstage(Path::new(&path))),
            Some(hunk) => {
                let Some(id) = file.hunks.get(hunk).map(|h| h.id.clone()) else {
                    return;
                };
                self.vcs_op(move |vcs| vcs.unstage_hunk(Path::new(&path), &id));
            }
        }
    }

    fn stage_all(&mut self) {
        let paths: Vec<String> = self
            .section_files(Section::Untracked)
            .iter()
            .chain(self.section_files(Section::Unstaged))
            .map(|file| file.path.clone())
            .collect();
        if paths.is_empty() {
            self.info("nothing to stage");
            return;
        }
        self.vcs_op(move |vcs| paths.iter().try_for_each(|path| vcs.stage(Path::new(path))));
    }

    fn unstage_all(&mut self) {
        let paths: Vec<String> = self
            .section_files(Section::Staged)
            .iter()
            .map(|file| file.path.clone())
            .collect();
        if paths.is_empty() {
            self.info("nothing staged");
            return;
        }
        self.vcs_op(move |vcs| {
            paths
                .iter()
                .try_for_each(|path| vcs.unstage(Path::new(path)))
        });
    }

    fn discard_at_cursor(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        let Some((_, file, _)) = self.row_file(&row) else {
            return;
        };
        let path = file.path.clone();
        self.modal = Some(Modal::Confirm {
            message: format!("Discard changes to {path}?"),
            on_confirm: PendingOp::Discard { path },
        });
    }

    fn stash_push(&mut self) {
        self.message = None;
        self.vcs_op(|vcs| vcs.stash_push(None));
        if self.message.is_none() {
            self.info("stashed changes");
        }
    }

    fn stash_pop(&mut self) {
        self.message = None;
        self.vcs_op(|vcs| vcs.stash_pop());
        if self.message.is_none() {
            self.info("popped latest stash");
        }
    }

    fn open_at_cursor(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        match &row {
            Row::Unpushed { index } => {
                let Some(oid) = self.status.unpushed.get(*index).map(|e| e.oid.clone()) else {
                    return;
                };
                self.open_commit_diff(&oid);
            }
            Row::Commit { index } => {
                let Some(oid) = self.status.recent.get(*index).map(|e| e.oid.clone()) else {
                    return;
                };
                self.open_commit_diff(&oid);
            }
            Row::Pr => self.open_pr_review(),
            // a branch row checks it out directly; the header opens the full picker
            Row::Branch { index } => {
                let Some(name) = self.status.branches.get(*index).map(|b| b.name.clone()) else {
                    return;
                };
                self.checkout_branch(&name);
            }
            Row::BranchesHeader { .. } => self.open_branch_list(BranchAction::Checkout),
            // a CI run opens its graph directly; the header opens the full Runs list
            Row::CiRun { index } => {
                self.runs_cursor = *index;
                self.open_selected_run();
            }
            Row::CiHeader { .. } => self.open_runs(),
            Row::RecentHeader { .. } => self.open_log(),
            // a section header opens the full review diff, starting the
            // walk at the section's first file (when the review covers it)
            Row::SectionHeader { section, .. } => {
                let section = *section;
                let review_model = self.review.model();
                let path = self
                    .section_files(section)
                    .iter()
                    .find(|f| review_model.files.iter().any(|m| m.path == f.path))
                    .map(|f| f.path.clone());
                self.open_working_tree_diff(path.as_deref());
            }
            // file/hunk/diff rows open the file; a dir row has no file: no-op
            row => {
                let Some(path) = self.row_file(row).map(|(_, file, _)| file.path.clone()) else {
                    return;
                };
                self.open_working_tree_file(&path);
            }
        }
    }

    fn toggle_viewed(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        let Some(path) = self.row_file(&row).map(|(_, file, _)| file.path.clone()) else {
            return;
        };
        let Some(hash) = self
            .review
            .model()
            .files
            .iter()
            .find(|f| f.path == path)
            .map(FileDiff::content_hash)
        else {
            self.info(format!("{path} is not part of the review diff"));
            return;
        };
        if self.review.session.is_viewed(&path, &hash) {
            self.review.session.unmark_viewed(&path);
        } else {
            self.review.session.mark_viewed(&path, &hash);
            // a viewed file reads as done: collapse its inline diffs
            for set in &mut self.status.expanded {
                set.remove(&path);
            }
        }
        if let Err(err) = self.review.save() {
            self.error(err.to_string());
        }
    }

    fn toggle_fold(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        match row {
            Row::SectionHeader { section, .. } => {
                if let Some(folded) = self.status.folded.get_mut(section.index()) {
                    *folded ^= true;
                }
                self.cursor_to_section_header(section);
            }
            // a directory folds/unfolds in place; its row stays under the cursor
            Row::Dir { section, path, .. } => {
                if let Some(set) = self.status.folded_dirs.get_mut(section.index())
                    && !set.remove(&path)
                {
                    set.insert(path.clone());
                }
                self.seat_cursor_on(
                    |row| matches!(row, Row::Dir { section: s, path: p, .. } if *s == section && *p == path),
                );
            }
            Row::File { section, index, .. } => {
                let Some(path) = self
                    .section_files(section)
                    .get(index)
                    .map(|f| f.path.clone())
                else {
                    return;
                };
                if let Some(set) = self.status.expanded.get_mut(section.index())
                    && !set.remove(&path)
                {
                    set.insert(path);
                }
            }
            Row::HunkHeader { section, file, .. } | Row::DiffLine { section, file, .. } => {
                let Some(path) = self
                    .section_files(section)
                    .get(file)
                    .map(|f| f.path.clone())
                else {
                    return;
                };
                if let Some(set) = self.status.expanded.get_mut(section.index()) {
                    set.remove(&path);
                }
                // collapsing from inside lands the cursor on the file row
                self.seat_cursor_on(
                    |row| matches!(row, Row::File { section: s, index, .. } if *s == section && *index == file),
                );
            }
            Row::UnpushedHeader { .. } | Row::Unpushed { .. } => {
                self.status.unpushed_folded ^= true;
                self.seat_cursor_on(|row| matches!(row, Row::UnpushedHeader { .. }));
            }
            Row::RecentHeader { .. } | Row::Commit { .. } => {
                self.status.recent_folded ^= true;
                self.seat_cursor_on(|row| matches!(row, Row::RecentHeader { .. }));
            }
            Row::Pr | Row::RepoDivider => {}
            Row::BranchesHeader { .. } | Row::Branch { .. } => {
                self.status.branches_folded ^= true;
                self.seat_cursor_on(|row| matches!(row, Row::BranchesHeader { .. }));
            }
            Row::CiHeader { .. } | Row::CiRun { .. } => {
                self.status.ci_folded ^= true;
                self.seat_cursor_on(|row| matches!(row, Row::CiHeader { .. }));
            }
        }
        self.clamp_cursor();
    }

    /// Move the cursor onto the first visible row matching `pred`, if any.
    /// Every fold toggle needs this re-seat once the row set it sits in shifts.
    fn seat_cursor_on(&mut self, pred: impl Fn(&Row) -> bool) {
        if let Some(position) = self.visible_rows().iter().position(pred) {
            self.status.cursor = position;
        }
    }

    fn cursor_to_section_header(&mut self, section: Section) {
        self.seat_cursor_on(
            |row| matches!(row, Row::SectionHeader { section: s, .. } if *s == section),
        );
    }

    /// Move the cursor to the next/previous row matching `target`.
    fn jump(&mut self, forward: bool, target: impl Fn(&Row) -> bool) {
        let rows = self.visible_rows();
        let position = if forward {
            rows.iter()
                .enumerate()
                .skip(self.status.cursor + 1)
                .find(|(_, row)| target(row))
                .map(|(index, _)| index)
        } else {
            rows.iter()
                .enumerate()
                .take(self.status.cursor)
                .rfind(|(_, row)| target(row))
                .map(|(index, _)| index)
        };
        if let Some(position) = position {
            self.status.cursor = position;
        }
    }

    pub(super) fn status_cursor_anchor(&self) -> Option<CursorAnchor> {
        let row = self.cursor_row()?;
        Some(match &row {
            Row::SectionHeader { section, .. } => CursorAnchor::Section(*section),
            Row::Dir { section, path, .. } => CursorAnchor::Dir {
                section: *section,
                path: path.clone(),
            },
            Row::UnpushedHeader { .. } => CursorAnchor::Unpushed,
            Row::Unpushed { index } => CursorAnchor::UnpushedCommit(*index),
            Row::RecentHeader { .. } => CursorAnchor::Recent,
            Row::Commit { index } => CursorAnchor::Commit(*index),
            Row::Pr => CursorAnchor::Pr,
            // furniture: the cursor never actually rests here
            Row::RepoDivider => return None,
            Row::BranchesHeader { .. } => CursorAnchor::Branches,
            Row::Branch { index } => CursorAnchor::BranchRow(*index),
            Row::CiHeader { .. } => CursorAnchor::Ci,
            Row::CiRun { index } => CursorAnchor::CiRun(*index),
            Row::File { .. } | Row::HunkHeader { .. } | Row::DiffLine { .. } => {
                let (section, file, hunk) = self.row_file(&row)?;
                CursorAnchor::File {
                    section,
                    path: file.path.clone(),
                    hunk,
                }
            }
        })
    }

    /// Re-seat the cursor after rows changed: exact hunk → same file in the
    /// same section → same path anywhere → the section header → clamp.
    #[allow(clippy::too_many_lines)] // one match arm per anchor kind, flat by design
    pub(super) fn restore_status_cursor(&mut self, anchor: Option<CursorAnchor>) {
        let Some(anchor) = anchor else {
            self.clamp_cursor();
            return;
        };
        let rows = self.visible_rows();
        let position = match &anchor {
            CursorAnchor::Section(section) => rows
                .iter()
                .position(|r| matches!(r, Row::SectionHeader { section: s, .. } if s == section)),
            CursorAnchor::Unpushed => rows
                .iter()
                .position(|r| matches!(r, Row::UnpushedHeader { .. })),
            CursorAnchor::UnpushedCommit(index) => rows
                .iter()
                .position(|r| matches!(r, Row::Unpushed { index: i } if i == index))
                .or_else(|| {
                    rows.iter()
                        .position(|r| matches!(r, Row::UnpushedHeader { .. }))
                }),
            CursorAnchor::Recent => rows
                .iter()
                .position(|r| matches!(r, Row::RecentHeader { .. })),
            CursorAnchor::Commit(index) => rows
                .iter()
                .position(|r| matches!(r, Row::Commit { index: i } if i == index))
                .or_else(|| {
                    rows.iter()
                        .position(|r| matches!(r, Row::RecentHeader { .. }))
                }),
            CursorAnchor::Pr => rows.iter().position(|r| matches!(r, Row::Pr)),
            CursorAnchor::Branches => rows
                .iter()
                .position(|r| matches!(r, Row::BranchesHeader { .. })),
            CursorAnchor::BranchRow(index) => rows
                .iter()
                .position(|r| matches!(r, Row::Branch { index: i } if i == index))
                .or_else(|| {
                    rows.iter()
                        .position(|r| matches!(r, Row::BranchesHeader { .. }))
                }),
            CursorAnchor::Ci => rows.iter().position(|r| matches!(r, Row::CiHeader { .. })),
            CursorAnchor::CiRun(index) => rows
                .iter()
                .position(|r| matches!(r, Row::CiRun { index: i } if i == index))
                .or_else(|| rows.iter().position(|r| matches!(r, Row::CiHeader { .. }))),
            // a folded dir survives a refresh by its path; fall back to the
            // section header when the directory is gone
            CursorAnchor::Dir { section, path } => rows
                .iter()
                .position(
                    |r| matches!(r, Row::Dir { section: s, path: p, .. } if s == section && p == path),
                )
                .or_else(|| {
                    rows.iter().position(
                        |r| matches!(r, Row::SectionHeader { section: s, .. } if s == section),
                    )
                }),
            CursorAnchor::File {
                section,
                path,
                hunk,
            } => {
                let file_at = |row: &Row| -> Option<(Section, usize)> {
                    match row {
                        Row::File { section, index, .. } => Some((*section, *index)),
                        _ => None,
                    }
                };
                let path_matches = |s: Section, index: usize| {
                    self.section_files(s)
                        .get(index)
                        .is_some_and(|f| f.path == *path)
                };
                let hunk_position = hunk.and_then(|h| {
                    rows.iter().position(|r| {
                        matches!(
                            r,
                            Row::HunkHeader { section: s, file, hunk } if s == section && *hunk == h && path_matches(*s, *file)
                        )
                    })
                });
                hunk_position
                    .or_else(|| {
                        rows.iter().position(|r| {
                            file_at(r)
                                .is_some_and(|(s, index)| s == *section && path_matches(s, index))
                        })
                    })
                    .or_else(|| {
                        rows.iter().position(|r| {
                            file_at(r).is_some_and(|(s, index)| path_matches(s, index))
                        })
                    })
                    .or_else(|| {
                        rows.iter().position(
                            |r| matches!(r, Row::SectionHeader { section: s, .. } if s == section),
                        )
                    })
            }
        };
        match position {
            Some(position) => self.status.cursor = position,
            None => self.clamp_cursor(),
        }
    }

    pub(super) fn clamp_cursor(&mut self) {
        let rows = self.visible_rows();
        let clamped = self.status.cursor.min(rows.len().saturating_sub(1));
        self.status.cursor = nearest_selectable(&rows, clamped, true);
    }
}

fn is_hunk_header(row: &Row) -> bool {
    matches!(row, Row::HunkHeader { .. })
}

fn is_section_header(row: &Row) -> bool {
    matches!(
        row,
        Row::SectionHeader { .. }
            | Row::UnpushedHeader { .. }
            | Row::BranchesHeader { .. }
            | Row::RecentHeader { .. }
            | Row::CiHeader { .. }
    )
}

/// Furniture rows the cursor must never land on.
fn is_selectable(row: &Row) -> bool {
    !matches!(row, Row::RepoDivider)
}

fn first_selectable_from(rows: &[Row], start: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .skip(start)
        .find(|(_, row)| is_selectable(row))
        .map(|(index, _)| index)
}

fn last_selectable_up_to(rows: &[Row], end: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .take(end + 1)
        .rev()
        .find(|(_, row)| is_selectable(row))
        .map(|(index, _)| index)
}

/// The selectable row nearest `index`, preferring `forward`'s direction and
/// falling back to the other one, so a divider (alone, doubled up, or at
/// either end) can never trap the cursor.
fn nearest_selectable(rows: &[Row], index: usize, forward: bool) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let index = index.min(rows.len() - 1);
    let found = if forward {
        first_selectable_from(rows, index).or_else(|| last_selectable_up_to(rows, index))
    } else {
        last_selectable_up_to(rows, index).or_else(|| first_selectable_from(rows, index))
    };
    found.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use diffler_core::source::ReviewSource;

    use super::super::Screen;
    use super::*;
    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, ctrl_key, key, standard_fixture, two_hunk_fixture};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app() -> (Fixture, App) {
        let fixture = standard_fixture();
        let app = App::new(fixture.review(), LoadedConfig::default());
        (fixture, app)
    }

    /// An app whose status file layout is forced to `layout`, overriding the
    /// default.
    fn app_with_status_layout(layout: crate::config::FileLayout) -> (Fixture, App) {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.status_file_layout = layout;
        let app = App::new(fixture.review(), loaded);
        (fixture, app)
    }

    /// Move the cursor onto the first row matching `pred`.
    fn cursor_to(app: &mut App, pred: impl Fn(&Row) -> bool) -> Row {
        let rows = app.visible_rows();
        let position = rows.iter().position(pred).expect("row present");
        app.status.cursor = position;
        rows.into_iter().nth(position).expect("row present")
    }

    fn file_row_in(section: Section) -> impl Fn(&Row) -> bool {
        move |row| matches!(row, Row::File { section: s, .. } if *s == section)
    }

    fn esc() -> AppEvent {
        AppEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    #[test]
    fn enter_on_a_ci_run_opens_its_graph() {
        use crate::ci::{CiRun, JobStatus, RunId};
        let (_fixture, mut app) = app();
        app.runs = vec![CiRun {
            id: RunId("1".into()),
            name: "CI".into(),
            title: String::new(),
            branch: "main".into(),
            commit: "abc".into(),
            author: String::new(),
            created: None,
            status: JobStatus::Running,
            url: None,
            remote: None,
        }];
        app.status.ci_folded = false;
        cursor_to(&mut app, |row| matches!(row, Row::CiRun { .. }));
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.screen(), crate::app::Screen::Graph);
        assert!(app.graph.is_some());
    }

    #[test]
    fn enter_on_the_pr_row_queues_the_head_fetch() {
        let (_fixture, mut app) = app();
        app.pr = Some(crate::ci::PullRequest {
            number: 7,
            title: "Add widgets".into(),
            url: None,
            head_ref: "feat/x".into(),
            author: String::new(),
            base_ref: "main".into(),
            head_oid: "0000000000000000000000000000000000000abc".into(),
        });
        cursor_to(&mut app, |row| matches!(row, Row::Pr));
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        // the head isn't local, so the open waits on a forge fetch
        let git = app.pending_git.take().expect("fetch queued");
        assert_eq!(git.argv[..2], ["git".to_owned(), "fetch".to_owned()]);
        assert!(
            git.argv.iter().any(|a| a == "refs/pull/7/head"),
            "{:?}",
            git.argv
        );
        assert_eq!(app.pending_pr_open.as_ref().map(|p| p.number), Some(7));
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let (_fixture, mut app) = app();
        // flat default: untracked (header + todo.md) + unstaged (header +
        // lib.rs) + staged (header + ci.yml) + the repo-band divider +
        // branches header + recent commits header: 9 rows
        assert_eq!(app.visible_rows().len(), 9);
        app.handle(key('k'));
        assert_eq!(app.status.cursor, 0, "MoveUp clamps at the top");
        for _ in 0..20 {
            app.handle(key('j'));
        }
        assert_eq!(app.status.cursor, 8, "MoveDown clamps at the last row");
    }

    #[test]
    fn gg_and_shift_g_jump_to_the_edges() {
        let (_fixture, mut app) = app();
        app.handle(key('G'));
        assert_eq!(app.status.cursor, app.visible_rows().len() - 1);
        app.handle(key('g'));
        app.handle(key('g'));
        assert_eq!(app.status.cursor, 0);
    }

    #[test]
    fn half_page_motions_step_by_the_viewport_and_clamp() {
        let (_fixture, mut app) = app();
        assert_eq!(app.visible_rows().len(), 9);
        // a half-page of a 4-row body is 2 rows
        app.status.viewport = 4;
        app.handle(ctrl_key('d'));
        assert_eq!(app.status.cursor, 2);
        app.handle(ctrl_key('d'));
        assert_eq!(app.status.cursor, 4);
        app.handle(ctrl_key('u'));
        assert_eq!(app.status.cursor, 2);
        // a tall viewport clamps to the last row, never past it
        app.status.viewport = 40;
        app.handle(ctrl_key('d'));
        assert_eq!(app.status.cursor, 8);
        app.handle(ctrl_key('u'));
        assert_eq!(app.status.cursor, 0);
    }

    #[test]
    fn fold_toggles_the_section_under_the_cursor() {
        let (_fixture, mut app) = app();
        // the untracked section holds one root-level file (todo.md)
        app.handle(key('\t'));
        assert!(app.is_folded(Section::Untracked));
        assert_eq!(app.visible_rows().len(), 8);
        app.handle(key('\t'));
        assert!(!app.is_folded(Section::Untracked));
        assert_eq!(app.visible_rows().len(), 9);
    }

    #[test]
    fn the_default_layout_lists_files_flat_with_no_dir_rows() {
        let (_fixture, app) = app();
        let rows = app.visible_rows();
        // the flat magit list emits no Dir rows at all
        assert!(
            !rows.iter().any(|r| matches!(r, Row::Dir { .. })),
            "flat list has no directory rows: {rows:?}"
        );
        // every file row sits at depth 0 (no tree indentation)
        assert!(
            rows.iter()
                .all(|r| !matches!(r, Row::File { depth, .. } if *depth != 0)),
            "flat file rows live at depth 0: {rows:?}"
        );
        // the nested unstaged file is still present, just without its src dir
        let unstaged_files = rows
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Row::File {
                        section: Section::Unstaged,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(unstaged_files, 1, "the src/lib.rs row is there: {rows:?}");
    }

    #[test]
    fn the_tree_layout_lists_its_files_as_a_directory_tree() {
        let (_fixture, app) = app_with_status_layout(crate::config::FileLayout::Tree);
        // unstaged holds src/lib.rs: a src dir row precedes the file row
        let rows = app.visible_rows();
        let unstaged: Vec<&Row> = rows
            .iter()
            .skip_while(|r| {
                !matches!(
                    r,
                    Row::SectionHeader {
                        section: Section::Unstaged,
                        ..
                    }
                )
            })
            .skip(1)
            .take_while(|r| {
                !matches!(
                    r,
                    Row::SectionHeader { .. }
                        | Row::RepoDivider
                        | Row::BranchesHeader { .. }
                        | Row::RecentHeader { .. }
                        | Row::CiHeader { .. }
                )
            })
            .collect();
        assert!(
            matches!(unstaged.first(), Some(Row::Dir { path, depth: 0, .. }) if path == "src"),
            "src dir row at depth 0: {unstaged:?}"
        );
        assert!(
            matches!(unstaged.get(1), Some(Row::File { depth: 1, .. })),
            "the file row nests under the dir: {unstaged:?}"
        );
    }

    #[test]
    fn tab_on_a_dir_row_folds_it_and_hides_its_files() {
        let (_fixture, mut app) = app_with_status_layout(crate::config::FileLayout::Tree);
        // cursor onto the src dir row in the unstaged section
        cursor_to(
            &mut app,
            |row| matches!(row, Row::Dir { path, .. } if path == "src"),
        );
        app.handle(key('\t'));
        assert!(app.is_dir_folded(Section::Unstaged, "src"));
        assert!(
            !app.visible_rows().iter().any(|r| matches!(
                r,
                Row::File {
                    section: Section::Unstaged,
                    ..
                }
            )),
            "folding src/ hid the file under it"
        );
        // the cursor stayed on the (still-visible) dir row
        assert!(matches!(
            app.visible_rows()[app.status.cursor],
            Row::Dir { path: ref p, .. } if p == "src"
        ));
        // tab again unfolds and the file returns
        app.handle(key('\t'));
        assert!(!app.is_dir_folded(Section::Unstaged, "src"));
        assert!(app.visible_rows().iter().any(|r| matches!(
            r,
            Row::File {
                section: Section::Unstaged,
                ..
            }
        )));
    }

    #[test]
    fn untracked_files_slot_into_their_tree_by_path() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    41\n}\n");
        fixture.commit_all("initial commit");
        // a new untracked file in a nested directory
        fixture.write("docs/api/intro.md", "# intro\n");
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.status_file_layout = crate::config::FileLayout::Tree;
        let app = App::new(fixture.review(), loaded);
        let rows = app.visible_rows();
        let kinds: Vec<String> = rows
            .iter()
            .skip_while(|r| {
                !matches!(
                    r,
                    Row::SectionHeader {
                        section: Section::Untracked,
                        ..
                    }
                )
            })
            .skip(1)
            .take_while(|r| {
                !matches!(
                    r,
                    Row::SectionHeader { .. }
                        | Row::RepoDivider
                        | Row::BranchesHeader { .. }
                        | Row::RecentHeader { .. }
                        | Row::CiHeader { .. }
                )
            })
            .map(|r| match r {
                Row::Dir { path, depth, .. } => format!("dir:{path}@{depth}"),
                Row::File { index, depth, .. } => format!("file:{index}@{depth}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                // docs/ and api/ are a single-child chain, collapsed to one row
                "dir:docs/api@0".to_owned(),
                "file:0@1".to_owned(),
            ],
            "the untracked file nests under the collapsed docs/api chain"
        );
    }

    #[test]
    fn tab_on_a_file_row_expands_its_inline_diff() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('\t'));
        assert!(app.is_expanded(Section::Unstaged, "src/lib.rs"));
        let rows = app.visible_rows();
        assert!(rows.iter().any(is_hunk_header), "hunk rows appear inline");
        assert!(
            rows.iter().any(|r| matches!(r, Row::DiffLine { .. })),
            "diff line rows appear inline"
        );
        app.handle(key('\t'));
        assert!(!app.is_expanded(Section::Unstaged, "src/lib.rs"));
    }

    #[test]
    fn tab_inside_an_expanded_diff_collapses_back_to_the_file_row() {
        let (_fixture, mut app) = app();
        let row = cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('\t'));
        app.handle(key('j'));
        app.handle(key('j'));
        assert!(matches!(
            app.visible_rows()[app.status.cursor],
            Row::DiffLine { .. }
        ));
        app.handle(key('\t'));
        assert!(!app.is_expanded(Section::Unstaged, "src/lib.rs"));
        assert_eq!(app.visible_rows()[app.status.cursor], row);
    }

    #[test]
    fn expansion_survives_refresh() {
        let (fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('\t'));
        fixture.write("another.md", "more\n");
        app.handle(ctrl_key('r'));
        assert!(app.is_expanded(Section::Unstaged, "src/lib.rs"));
        assert!(app.visible_rows().iter().any(is_hunk_header));
    }

    #[test]
    fn hunk_jumps_move_between_hunk_headers() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('\t'));
        app.handle(key(']'));
        let first = app.status.cursor;
        assert!(is_hunk_header(&app.visible_rows()[first]));
        app.handle(key(']'));
        let second = app.status.cursor;
        assert!(second > first, "second hunk header is further down");
        assert!(is_hunk_header(&app.visible_rows()[second]));
        app.handle(key('['));
        assert_eq!(app.status.cursor, first);
    }

    #[test]
    fn section_jumps_move_between_headers() {
        let (_fixture, mut app) = app();
        app.handle(ctrl_key('n'));
        assert!(matches!(
            app.visible_rows()[app.status.cursor],
            Row::SectionHeader {
                section: Section::Unstaged,
                ..
            }
        ));
        app.handle(ctrl_key('n'));
        app.handle(ctrl_key('n'));
        assert!(matches!(
            app.visible_rows()[app.status.cursor],
            Row::BranchesHeader { .. }
        ));
        app.handle(ctrl_key('p'));
        assert!(matches!(
            app.visible_rows()[app.status.cursor],
            Row::SectionHeader {
                section: Section::Staged,
                ..
            }
        ));
    }

    #[test]
    fn stage_on_a_file_row_moves_it_to_staged() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('s'));
        app.settle_refresh();
        assert_eq!(app.section_files(Section::Unstaged).len(), 0);
        let staged: Vec<_> = app
            .section_files(Section::Staged)
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert!(staged.contains(&"src/lib.rs"));
    }

    #[test]
    fn stage_on_an_untracked_row_moves_it_to_staged() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Untracked));
        app.handle(key('s'));
        app.settle_refresh();
        assert_eq!(app.section_files(Section::Untracked).len(), 0);
        assert!(
            app.section_files(Section::Staged)
                .iter()
                .any(|f| f.path == "todo.md")
        );
    }

    #[test]
    fn stage_in_the_staged_section_hints_already_staged() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Staged));
        app.handle(key('s'));
        let message = app.message.clone().expect("message");
        assert_eq!(message.severity, super::super::Severity::Info);
        assert!(message.text.contains("already staged"));
        assert_eq!(app.section_files(Section::Staged).len(), 1);
    }

    #[test]
    fn unstage_outside_the_staged_section_hints() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('u'));
        let message = app.message.expect("message");
        assert!(message.text.contains("not staged"));
    }

    #[test]
    fn unstage_moves_a_staged_file_back() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Staged));
        app.handle(key('u'));
        app.settle_refresh();
        assert_eq!(app.section_files(Section::Staged).len(), 0);
        // ci.yml was a staged new file: unstaging makes it untracked again
        assert!(
            app.section_files(Section::Untracked)
                .iter()
                .any(|f| f.path == "ci.yml")
        );
    }

    #[test]
    fn stage_one_hunk_splits_the_file_across_sections() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('\t'));
        app.handle(key(']'));
        assert!(is_hunk_header(&app.visible_rows()[app.status.cursor]));
        app.handle(key('s'));
        app.settle_refresh();
        let in_section = |section: Section| {
            app.section_files(section)
                .iter()
                .any(|f| f.path == "data.txt")
        };
        assert!(in_section(Section::Staged), "staged hunk lands in staged");
        assert!(in_section(Section::Unstaged), "other hunk stays unstaged");
    }

    #[test]
    fn stage_all_and_unstage_all_move_everything() {
        let (_fixture, mut app) = app();
        app.handle(key('S'));
        app.settle_refresh();
        assert_eq!(app.section_files(Section::Untracked).len(), 0);
        assert_eq!(app.section_files(Section::Unstaged).len(), 0);
        assert_eq!(app.section_files(Section::Staged).len(), 3);
        app.handle(key('U'));
        app.settle_refresh();
        assert_eq!(app.section_files(Section::Staged).len(), 0);
    }

    #[test]
    fn discard_asks_for_confirmation_and_cancels_on_n() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('x'));
        let Some(Modal::Confirm { message, .. }) = &app.modal else {
            panic!("expected a confirm modal");
        };
        assert!(message.contains("src/lib.rs"));
        // while the modal is up, normal keys are swallowed
        let cursor = app.status.cursor;
        app.handle(key('j'));
        assert_eq!(app.status.cursor, cursor);
        app.handle(key('n'));
        assert_eq!(app.modal, None);
        assert_eq!(app.section_files(Section::Unstaged).len(), 1);
    }

    #[test]
    fn discard_confirmed_with_y_drops_the_change() {
        let (fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('x'));
        app.handle(key('y'));
        app.settle_refresh();
        assert_eq!(app.modal, None);
        assert_eq!(app.section_files(Section::Unstaged).len(), 0);
        let content = std::fs::read_to_string(fixture.root.join("src/lib.rs")).unwrap();
        assert!(content.contains("41"), "worktree restored to HEAD");
    }

    #[test]
    fn discard_an_untracked_file_deletes_it() {
        let (fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Untracked));
        app.handle(key('x'));
        app.handle(key('y'));
        app.settle_refresh();
        assert!(!fixture.root.join("todo.md").exists());
        assert_eq!(app.section_files(Section::Untracked).len(), 0);
    }

    #[test]
    fn escape_cancels_the_confirm_modal() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Untracked));
        app.handle(key('x'));
        app.handle(esc());
        assert_eq!(app.modal, None);
        assert_eq!(app.section_files(Section::Untracked).len(), 1);
    }

    #[test]
    fn open_pushes_a_diff_screen_scoped_to_the_cursor_file() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff view");
        assert_eq!(diff.source, ReviewSource::WorkingTree);
        assert_eq!(
            diff.focus,
            super::super::Pane::Diff,
            "a file row focuses the diff"
        );
        let path = app.diff_cursor_path().expect("cursor on the scoped file");
        assert_eq!(path, "src/lib.rs");
    }

    #[test]
    fn open_on_a_section_header_starts_the_review_walk_at_its_first_file() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, |row| {
            matches!(
                row,
                Row::SectionHeader {
                    section: Section::Staged,
                    ..
                }
            )
        });
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff view");
        assert_eq!(diff.source, ReviewSource::WorkingTree);
        assert_eq!(
            diff.focus,
            super::super::Pane::List,
            "a section header focuses the sidebar"
        );
        assert_eq!(
            app.diff_cursor_path().as_deref(),
            Some("ci.yml"),
            "selection on the staged section's first review file"
        );
        // unscoped: the whole review diff is in the view
        let model = app.diff.as_ref().unwrap().model(&app.review);
        assert!(model.files.iter().any(|f| f.path == "src/lib.rs"));
    }

    #[test]
    fn open_on_a_header_whose_files_left_the_review_lands_at_the_top() {
        let (_fixture, mut app) = app();
        // simulate the staged file leaving the review diff (e.g. a stage
        // reverted in the worktree between refreshes)
        app.review.model_mut().files.retain(|f| f.path != "ci.yml");
        cursor_to(&mut app, |row| {
            matches!(
                row,
                Row::SectionHeader {
                    section: Section::Staged,
                    ..
                }
            )
        });
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Diff);
        assert_eq!(
            app.diff.as_ref().expect("diff view").cursor,
            0,
            "no section file in the review diff: open at the top"
        );
    }

    #[test]
    fn shift_d_opens_the_full_review_diff_at_the_top() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Staged));
        app.handle(key('D'));
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff view");
        assert_eq!(diff.source, ReviewSource::WorkingTree);
        assert_eq!(diff.cursor, 0, "unscoped open starts at the top");
    }

    #[test]
    fn open_on_a_commit_row_pushes_a_commit_diff() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, |row| matches!(row, Row::RecentHeader { .. }));
        app.handle(key('\t'));
        app.handle(key('j'));
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff view");
        let ReviewSource::Commit { oid } = &diff.source else {
            panic!("expected a commit source, got {:?}", diff.source);
        };
        assert_eq!(oid.len(), 40);
    }

    #[test]
    fn viewed_toggle_persists_to_disk_and_collapses_the_file() {
        let (fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('\t'));
        assert!(app.is_expanded(Section::Unstaged, "src/lib.rs"));
        app.handle(key('v'));
        assert!(app.is_path_viewed("src/lib.rs"));
        assert!(
            !app.is_expanded(Section::Unstaged, "src/lib.rs"),
            "marking viewed collapses the inline diff"
        );
        let reloaded = diffler_core::store::load(&fixture.root).unwrap();
        assert!(reloaded.viewed.contains_key("src/lib.rs"));

        app.handle(key('v'));
        assert!(!app.is_path_viewed("src/lib.rs"));
        let reloaded = diffler_core::store::load(&fixture.root).unwrap();
        assert!(!reloaded.viewed.contains_key("src/lib.rs"));
    }

    #[test]
    fn viewed_counts_feed_the_status_bar() {
        let (_fixture, mut app) = app();
        assert_eq!(app.viewed_counts(), (3, 0));
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('v'));
        assert_eq!(app.viewed_counts(), (3, 1));
    }

    #[test]
    fn viewed_counts_follow_the_open_commit_diff() {
        let (_fixture, mut app) = app();
        let oid = app.status.recent[0].oid.clone();
        app.open_commit_diff(&oid);
        let total = app
            .diff
            .as_ref()
            .and_then(|d| d.commit_model.as_ref())
            .expect("commit model")
            .files
            .len();
        assert_eq!(app.viewed_counts(), (total, 0));
        app.handle(key('v'));
        assert_eq!(
            app.viewed_counts(),
            (total, 1),
            "counts read the commit source, not the working tree"
        );
    }

    #[test]
    fn e_requests_the_editor_on_the_cursor_file() {
        let (fixture, mut app) = app();
        // pin the editor through config so the test ignores $EDITOR
        app.config.editor.command = Some("vim".to_owned());
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('e'));
        let request = app.pending_editor.clone().expect("editor request");
        assert_eq!(
            request.purpose,
            crate::editor::EditorPurpose::OpenFile {
                path: "src/lib.rs".to_owned(),
            }
        );
        let absolute = fixture.root.join("src/lib.rs");
        assert_eq!(
            request.cmd,
            vec!["vim".to_owned(), absolute.to_string_lossy().into_owned()],
            "a file row opens at the top: no line argument"
        );
    }

    #[test]
    fn e_on_a_section_header_hints() {
        let (_fixture, mut app) = app();
        app.status.cursor = 0;
        app.handle(key('e'));
        assert_eq!(app.pending_editor, None);
        let message = app.message.expect("message");
        assert!(message.text.contains("no file under the cursor"));
    }

    #[test]
    fn e_on_an_expanded_diff_line_passes_the_line_number() {
        let (fixture, mut app) = app();
        app.config.editor.command = Some("vim".to_owned());
        // expand the unstaged file's inline diff
        cursor_to(&mut app, file_row_in(Section::Unstaged));
        app.handle(key('\t'));
        // find the first DiffLine and what line number it carries
        let rows = app.visible_rows();
        let (diff_line_pos, row) = rows
            .iter()
            .enumerate()
            .find(|(_, r)| matches!(r, Row::DiffLine { .. }))
            .expect("inline diff line present");
        let Row::DiffLine {
            section,
            file,
            hunk,
            line,
        } = *row
        else {
            unreachable!()
        };
        let expected_line_no = app
            .section_files(section)
            .get(file)
            .and_then(|f| f.hunks.get(hunk))
            .and_then(|h| h.lines.get(line))
            .and_then(|l| l.new_no.or(l.old_no))
            .expect("diff line has a line number");
        app.status.cursor = diff_line_pos;
        app.handle(key('e'));
        let request = app.pending_editor.clone().expect("editor request");
        let absolute = fixture.root.join("src/lib.rs");
        let expected_arg = format!("+{expected_line_no}");
        assert!(
            request.cmd.contains(&expected_arg),
            "expected {expected_arg} in {:?}",
            request.cmd
        );
        assert!(
            request
                .cmd
                .contains(&absolute.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn refresh_picks_up_new_files() {
        let (fixture, mut app) = app();
        assert_eq!(app.section_files(Section::Untracked).len(), 1);
        fixture.write("another.md", "more\n");
        app.handle(ctrl_key('r'));
        app.settle_refresh();
        assert_eq!(app.section_files(Section::Untracked).len(), 2);
    }

    #[test]
    fn cursor_anchor_survives_refresh() {
        let (fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Staged));
        // a new untracked file shifts every row below the untracked section
        fixture.write("aaa.md", "first\n");
        app.handle(ctrl_key('r'));
        app.settle_refresh();
        let Some((section, file, _)) = app
            .visible_rows()
            .get(app.status.cursor)
            .cloned()
            .and_then(|row| app.row_file(&row))
            .map(|(s, f, h)| (s, f.path.clone(), h))
        else {
            panic!("cursor should still be on a file row");
        };
        assert_eq!(section, Section::Staged);
        assert_eq!(file, "ci.yml");
    }

    #[test]
    fn cursor_falls_back_to_the_section_when_its_file_leaves() {
        let (fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Untracked));
        fixture.stage("todo.md");
        app.handle(ctrl_key('r'));
        app.settle_refresh();
        // todo.md moved to staged: the anchor follows the path there
        let row = app.visible_rows()[app.status.cursor].clone();
        let (section, file, _) = app.row_file(&row).expect("file row");
        assert_eq!(section, Section::Staged);
        assert_eq!(file.path, "todo.md");
    }

    #[test]
    fn recent_commits_are_cached_and_folded_by_default() {
        let (_fixture, mut app) = app();
        assert_eq!(app.status.recent.len(), 1);
        assert!(app.status.recent_folded);
        cursor_to(&mut app, |row| matches!(row, Row::RecentHeader { .. }));
        app.handle(key('\t'));
        assert!(!app.status.recent_folded);
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| matches!(row, Row::Commit { .. }))
        );
    }

    #[test]
    fn refresh_updates_the_recent_commit_cache() {
        let (fixture, mut app) = app();
        fixture.write("notes.txt", "alpha\nbeta\n");
        fixture.commit_all("second commit");
        app.handle(ctrl_key('r'));
        app.settle_refresh();
        assert_eq!(app.status.recent.len(), 2);
        assert_eq!(app.status.recent[0].subject, "second commit");
    }

    #[test]
    fn unpushed_section_lists_commits_ahead_of_the_upstream_before_recent() {
        let fixture = standard_fixture();
        fixture.track("main", "HEAD");
        fixture.write("shipped_one.rs", "pub fn one() {}\n");
        fixture.commit_all("first unpushed");
        fixture.write("shipped_two.rs", "pub fn two() {}\n");
        fixture.commit_all("second unpushed");
        let app = App::new(fixture.review(), LoadedConfig::default());
        assert_eq!(app.status.unpushed.len(), 2);
        assert_eq!(app.status.unpushed[0].subject, "second unpushed");
        assert!(!app.status.unpushed_folded);
        let rows = app.visible_rows();
        let unpushed_at = rows
            .iter()
            .position(|r| matches!(r, Row::UnpushedHeader { count: 2 }))
            .expect("unpushed header with both commits");
        let recent_at = rows
            .iter()
            .position(|r| matches!(r, Row::RecentHeader { .. }))
            .expect("recent header");
        assert!(unpushed_at < recent_at, "unpushed comes before recent");
    }

    #[test]
    fn unpushed_section_is_absent_with_nothing_ahead() {
        let (_fixture, app) = app();
        assert!(app.status.unpushed.is_empty());
        assert!(
            !app.visible_rows()
                .iter()
                .any(|row| matches!(row, Row::UnpushedHeader { .. }))
        );
    }

    #[test]
    fn recent_commits_exclude_the_unpushed_oids() {
        let fixture = standard_fixture();
        fixture.track("main", "HEAD");
        fixture.write("shipped.rs", "pub fn shipped() {}\n");
        fixture.commit_all("only here");
        let app = App::new(fixture.review(), LoadedConfig::default());
        assert_eq!(app.status.unpushed.len(), 1);
        assert_eq!(app.status.unpushed[0].subject, "only here");
        assert!(
            app.status.recent.iter().all(|e| e.subject != "only here"),
            "recent excludes the unpushed commit: {:?}",
            app.status.recent
        );
        assert!(
            app.status
                .recent
                .iter()
                .any(|e| e.subject == "initial commit"),
            "recent still carries the already-pushed commit"
        );
    }

    #[test]
    fn pushing_moves_a_commit_from_unpushed_into_recent() {
        let fixture = standard_fixture();
        fixture.track("main", "HEAD");
        fixture.write("shipped.rs", "pub fn shipped() {}\n");
        fixture.commit_all("only here");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        assert_eq!(app.status.unpushed.len(), 1);
        fixture.track("main", "HEAD");
        app.handle(ctrl_key('r'));
        app.settle_refresh();
        assert!(app.status.unpushed.is_empty());
        assert!(
            app.status.recent.iter().any(|e| e.subject == "only here"),
            "the pushed commit lands in recent rather than vanishing: {:?}",
            app.status.recent
        );
    }

    #[test]
    fn unpushed_commits_do_not_eat_into_the_recent_list() {
        let fixture = standard_fixture();
        fixture.track("main", "HEAD");
        let mut config = LoadedConfig::default();
        config.config.ui.recent_commits = 2;
        for n in 0..4 {
            fixture.write(&format!("ahead{n}.rs"), "pub fn ahead() {}\n");
            fixture.commit_all(&format!("unpushed {n}"));
        }
        let app = App::new(fixture.review(), config);
        assert_eq!(app.status.unpushed.len(), 4);
        assert_eq!(
            app.status.recent.len(),
            1,
            "the fixture has one pushed commit and all of it should survive: {:?}",
            app.status.recent
        );
        assert_eq!(app.status.recent[0].subject, "initial commit");
    }

    #[test]
    fn branches_are_sorted_newest_tip_first() {
        let fixture = standard_fixture();
        // the fixture's commits share one fixed time (for stable oids), so
        // "old" is branched off before main is advanced with a later one
        fixture.branch("old");
        let sig = git2::Signature::new("test", "test@test", &git2::Time::new(1_700_000_100, 0))
            .expect("sig");
        diffler_core::test_git::commit_all(&fixture.repo, "advance main", &sig);
        let app = App::new(fixture.review(), LoadedConfig::default());
        let names: Vec<&str> = app
            .status
            .branches
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(names, vec!["main", "old"], "newest tip first: {names:?}");
    }

    #[test]
    fn the_checked_out_branch_leads_even_when_its_tip_is_older() {
        let fixture = standard_fixture();
        fixture.branch("newer");
        fixture.checkout("newer");
        let sig = git2::Signature::new("test", "test@test", &git2::Time::new(1_700_000_100, 0))
            .expect("sig");
        diffler_core::test_git::commit_all(&fixture.repo, "advance newer", &sig);
        fixture.checkout("main");
        let app = App::new(fixture.review(), LoadedConfig::default());
        let names: Vec<&str> = app
            .status
            .branches
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["main", "newer"],
            "head leads so the cap cannot drop your own branch: {names:?}"
        );
    }

    #[test]
    fn branches_are_capped_at_the_inline_limit() {
        let fixture = standard_fixture();
        for n in 0..(super::BRANCHES_INLINE_LIMIT + 2) {
            fixture.branch(&format!("b{n}"));
        }
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let total = app.status.branches.len();
        assert!(total > super::BRANCHES_INLINE_LIMIT);
        app.status.branches_folded = false;
        let rows = app.visible_rows();
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::BranchesHeader { count } if *count == total)),
            "the header counts every branch, so a cut list never reads as the total: {rows:?}"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, Row::Branch { .. }))
                .count(),
            super::BRANCHES_INLINE_LIMIT,
            "only the newest fit inline: {rows:?}"
        );
    }

    #[test]
    fn branches_group_is_folded_by_default_and_tab_unfolds_it() {
        let fixture = standard_fixture();
        fixture.branch("feat/topic");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        assert!(app.status.branches_folded);
        assert!(
            !app.visible_rows()
                .iter()
                .any(|row| matches!(row, Row::Branch { .. }))
        );
        cursor_to(&mut app, |row| matches!(row, Row::BranchesHeader { .. }));
        app.handle(key('\t'));
        assert!(!app.status.branches_folded);
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| matches!(row, Row::Branch { .. }))
        );
    }

    #[test]
    fn enter_on_a_branch_row_checks_it_out() {
        let fixture = standard_fixture();
        fixture.branch("feat/topic");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.status.branches_folded = false;
        let target = app
            .status
            .branches
            .iter()
            .position(|b| b.name == "feat/topic")
            .expect("the new branch is loaded");
        cursor_to(
            &mut app,
            |row| matches!(row, Row::Branch { index } if *index == target),
        );
        app.handle(key('\n'));
        assert_eq!(
            app.review.vcs.head().expect("head").branch.as_deref(),
            Some("feat/topic")
        );
    }

    #[test]
    fn checkout_on_the_current_branch_is_a_noop_info_message() {
        let (_fixture, mut app) = app();
        app.checkout_branch("main");
        let message = app.message.clone().expect("message");
        assert_eq!(message.severity, super::super::Severity::Info);
        assert!(message.text.contains("already on main"), "{message:?}");
    }

    #[test]
    fn j_and_k_skip_the_repo_divider() {
        let (_fixture, mut app) = app();
        let divider_at = app
            .visible_rows()
            .iter()
            .position(|row| matches!(row, Row::RepoDivider))
            .expect("the repo band is present with at least the current branch");
        app.status.cursor = divider_at - 1;
        app.handle(key('j'));
        assert_ne!(
            app.status.cursor, divider_at,
            "j never lands on the divider"
        );
        assert!(app.status.cursor > divider_at - 1);

        app.status.cursor = divider_at + 1;
        app.handle(key('k'));
        assert_ne!(
            app.status.cursor, divider_at,
            "k never lands on the divider"
        );
    }

    #[test]
    fn enter_on_the_branches_header_opens_the_full_picker() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, |row| matches!(row, Row::BranchesHeader { .. }));
        app.handle(key('\n'));
        assert!(matches!(app.modal, Some(Modal::BranchList { .. })));
    }

    #[test]
    fn enter_on_the_recent_header_opens_the_log() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, |row| matches!(row, Row::RecentHeader { .. }));
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Log);
    }

    #[test]
    fn enter_on_the_ci_header_opens_the_runs_screen() {
        use crate::ci::{CiRun, JobStatus, RunId};
        let (_fixture, mut app) = app();
        app.ci_remotes = vec![crate::app::CiRemote {
            name: "origin".to_owned(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::GitHub,
                host: None,
            },
            url: None,
        }];
        app.runs = vec![CiRun {
            id: RunId("1".into()),
            name: "CI".into(),
            title: String::new(),
            branch: "main".into(),
            commit: "abc".into(),
            author: String::new(),
            created: None,
            status: JobStatus::Ok,
            url: None,
            remote: None,
        }];
        cursor_to(&mut app, |row| matches!(row, Row::CiHeader { .. }));
        app.handle(key('\n'));
        assert_eq!(app.screen(), Screen::Runs);
    }

    #[test]
    fn pr_row_sits_in_the_branch_band_above_the_divider() {
        let (_fixture, mut app) = app();
        app.pr = Some(crate::ci::PullRequest {
            number: 7,
            title: "Add widgets".into(),
            url: None,
            head_ref: "feat/x".into(),
            author: String::new(),
            base_ref: "main".into(),
            head_oid: "abc".into(),
        });
        let rows = app.visible_rows();
        let pr_at = rows
            .iter()
            .position(|r| matches!(r, Row::Pr))
            .expect("pr row");
        let divider_at = rows
            .iter()
            .position(|r| matches!(r, Row::RepoDivider))
            .expect("divider row");
        assert!(
            pr_at < divider_at,
            "the PR row precedes the repo-band divider"
        );
    }

    #[test]
    fn a_pr_landing_late_leaves_the_cursor_on_the_same_row() {
        let (_fixture, mut app) = app();
        cursor_to(&mut app, |row| matches!(row, Row::RecentHeader { .. }));
        app.handle(AppEvent::CiPr(Some(crate::ci::PullRequest {
            number: 7,
            title: "Add widgets".into(),
            url: None,
            head_ref: "feat/x".into(),
            author: String::new(),
            base_ref: "main".into(),
            head_oid: "abc".into(),
        })));
        assert!(
            matches!(app.cursor_row(), Some(Row::RecentHeader { .. })),
            "the PR row inserts above the repo band without dragging the cursor: {:?}",
            app.cursor_row()
        );
    }

    fn type_query(app: &mut App, query: &str) {
        app.handle(key('/'));
        for c in query.chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
    }

    #[test]
    fn slash_search_moves_the_cursor_to_a_matching_file_row() {
        let (_fixture, mut app) = app();
        type_query(&mut app, "lib");
        let row = app.visible_rows()[app.status.cursor].clone();
        let (_, file, _) = app.row_file(&row).expect("cursor on a file row");
        assert_eq!(file.path, "src/lib.rs");
    }

    #[test]
    fn search_next_and_prev_cycle_status_matches() {
        let (_fixture, mut app) = app();
        // "changes" hits the Unstaged and Staged section titles
        type_query(&mut app, "changes");
        let section_at = |app: &App| match app.visible_rows()[app.status.cursor] {
            Row::SectionHeader { section, .. } => section,
            ref other => panic!("expected a section header, got {other:?}"),
        };
        assert_eq!(section_at(&app), Section::Unstaged);
        app.handle(key('n'));
        assert_eq!(section_at(&app), Section::Staged);
        app.handle(key('n'));
        assert_eq!(section_at(&app), Section::Unstaged, "next wraps");
        app.handle(key('N'));
        assert_eq!(section_at(&app), Section::Staged, "prev wraps back");
    }

    #[test]
    fn escape_clears_a_committed_status_search() {
        let (_fixture, mut app) = app();
        type_query(&mut app, "lib");
        assert!(app.search.is_some());
        app.handle(esc());
        assert!(app.search.is_none());
    }

    #[test]
    fn partially_staged_file_expanded_in_both_sections_settles() {
        // the same path carries different content in Unstaged and Staged, so
        // each side needs its own cache entry: a path-keyed cache would make
        // the two sections evict each other and re-enrich forever
        let fixture = standard_fixture();
        fixture.stage("src/lib.rs");
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    43\n}\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        for section in [Section::Unstaged, Section::Staged] {
            app.status.expanded[section.index()].insert("src/lib.rs".to_owned());
        }

        app.queue_enrich_status_expanded();
        assert_eq!(app.pending_enrich.len(), 2, "one job per section");
        app.enrich_now();
        app.queue_enrich_status_expanded();
        assert!(
            app.pending_enrich.is_empty(),
            "both sections stay settled once their outcomes land"
        );
    }
}
