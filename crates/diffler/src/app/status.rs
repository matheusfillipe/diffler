//! Status screen state and handlers: neogit-style sections with inline
//! diff expansion, folding, stage/unstage/discard, and cursor preservation
//! across refreshes.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use diffler_core::model::FileDiff;
use diffler_core::vcs::{LogEntry, NetworkOp};

use super::{App, BranchAction, FileHighlights, Modal, PendingOp};
use crate::config::FileLayout;
use crate::keymap::Action;
use crate::tree::{self, TreeNode, TreeRow};

/// Heading for the trailing recent-commits section, shared by the renderer and
/// the search labels so a `/` match lines up with the displayed text.
pub(crate) const RECENT_TITLE: &str = "Recent commits";

/// Heading for the leading CI-runs section (when a provider is detected).
pub(crate) const CI_TITLE: &str = "CI runs";

/// How many recent runs the inline status section shows (the full list lives on
/// the Runs screen).
const CI_INLINE_LIMIT: usize = 5;

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
/// rows, file rows, and — when a file is expanded inline — hunk headers and
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
    RecentHeader {
        count: usize,
    },
    Commit {
        index: usize,
    },
    /// Header of the leading CI-runs section.
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
    Recent,
    Commit(usize),
    Ci,
    CiRun(usize),
}

/// All state owned by the status screen.
pub struct StatusView {
    pub cursor: usize,
    pub folded: [bool; 3],
    pub recent: Vec<LogEntry>,
    pub recent_folded: bool,
    /// Whether the leading CI-runs section is collapsed.
    pub ci_folded: bool,
    /// Body height of the last render, so half-page motions step by a screenful.
    pub(crate) viewport: u16,
    /// Per-section set of file paths whose inline diff is expanded.
    expanded: [BTreeSet<String>; 3],
    /// Per-section set of folded directory paths in that section's file tree.
    folded_dirs: [BTreeSet<String>; 3],
    /// Per-section set of file paths whose inline diff has been enriched with
    /// intra-line emphasis, so the per-file enrichment runs once. Cleared
    /// when the status sections are rebuilt (refresh).
    enriched: [BTreeSet<String>; 3],
    /// Per-file syntax spans for inline diffs, keyed by path and validated by
    /// the both-sides content hash. Filled lazily, only for expanded files.
    pub(crate) highlights: HashMap<String, FileHighlights>,
    /// Last render's body rect, line scroll, and per-rendered-line row index
    /// (rows vary in height, so a screen row maps back to a `visible_rows`
    /// index only through this table). Drives mouse hit-testing.
    pub(crate) body: ratatui::layout::Rect,
    pub(crate) scroll: u16,
    pub(crate) line_rows: Vec<Option<usize>>,
}

impl StatusView {
    pub(super) fn new(recent: Vec<LogEntry>) -> Self {
        Self {
            cursor: 0,
            folded: [false; 3],
            recent,
            recent_folded: true,
            ci_folded: false,
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
            FileLayout::List => files
                .iter()
                .enumerate()
                .map(|(index, file)| TreeRow {
                    depth: 0,
                    node: TreeNode::File {
                        index,
                        name: file.path.clone(),
                    },
                })
                .collect(),
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
            // List renders a degenerate tree — one File row per file at depth 0,
            // no Dir rows — so the same cursor/navigation model serves both
            // layouts; Tree groups files under collapsible directory rows.
            for tree_row in self.section_layout_rows(section, files) {
                match tree_row.node {
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
        if !self.status.recent.is_empty() {
            rows.push(Row::RecentHeader {
                count: self.status.recent.len(),
            });
            if !self.status.recent_folded {
                rows.extend((0..self.status.recent.len()).map(|index| Row::Commit { index }));
            }
        }
        // CI runs trail the view so the section appearing after its async fetch
        // (or refreshing) never shifts the rows above it or moves the cursor
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
    /// diff rows are left out — the diff view is where code is searched.
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
            Row::RecentHeader { .. } => RECENT_TITLE.to_owned(),
            Row::Dir { name, .. } => name.clone(),
            Row::File { section, index, .. } => self
                .status_file_name(self.section_files(*section).get(*index)?)
                .to_owned(),
            Row::Commit { index } => self.status.recent.get(*index)?.subject.clone(),
            Row::CiHeader { .. } => CI_TITLE.to_owned(),
            Row::CiRun { index } => self.runs.get(*index)?.name.clone(),
            Row::HunkHeader { .. } | Row::DiffLine { .. } => return None,
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

    /// Enrich every currently-expanded inline diff with intra-line emphasis
    /// before the status screen renders. Memoized per section/path so the
    /// pairing runs once per expanded file. Called by the renderer.
    pub(crate) fn enrich_status_expanded(&mut self) {
        for section in Section::ALL {
            let index = section.index();
            let model = match section {
                Section::Untracked => &mut self.review.status.untracked,
                Section::Unstaged => &mut self.review.status.unstaged,
                Section::Staged => &mut self.review.status.staged,
            };
            let (Some(expanded), Some(enriched)) = (
                self.status.expanded.get(index),
                self.status.enriched.get_mut(index),
            ) else {
                continue;
            };
            for file in &mut model.files {
                if expanded.contains(&file.path) && enriched.insert(file.path.clone()) {
                    diffler_core::pairing::enrich_file(file);
                }
            }
        }
    }

    /// Drive `fill` over every currently-expanded inline diff's file and its
    /// syntax cache, so the renderer can highlight expanded files lazily —
    /// only expanded files are touched. The UI supplies `fill` (the same
    /// per-file highlighter the diff pane uses), keeping the highlighter in
    /// the render layer.
    pub(crate) fn ensure_status_highlights(
        &mut self,
        mut fill: impl FnMut(&mut HashMap<String, FileHighlights>, &FileDiff),
    ) {
        let cache = &mut self.status.highlights;
        for section in Section::ALL {
            let index = section.index();
            let model = match section {
                Section::Untracked => &self.review.status.untracked,
                Section::Unstaged => &self.review.status.unstaged,
                Section::Staged => &self.review.status.staged,
            };
            let Some(expanded) = self.status.expanded.get(index) else {
                continue;
            };
            for file in &model.files {
                if expanded.contains(&file.path) {
                    fill(cache, file);
                }
            }
        }
    }

    /// Move the cursor by half a screenful, clamped to the visible rows.
    fn status_page(&mut self, down: bool, full: bool) {
        // before the first render the height is unknown; a typical terminal
        // is a fine guess
        let viewport = if self.status.viewport == 0 {
            40
        } else {
            usize::from(self.status.viewport)
        };
        let step = if full {
            viewport.saturating_sub(1).max(1)
        } else {
            (viewport / 2).max(1)
        };
        if down {
            let last = self.visible_rows().len().saturating_sub(1);
            self.status.cursor = (self.status.cursor + step).min(last);
        } else {
            self.status.cursor = self.status.cursor.saturating_sub(step);
        }
    }

    pub(super) fn status_mouse(&mut self, gesture: super::MouseGesture) {
        use super::MouseGesture;
        match gesture {
            MouseGesture::Scroll { down, .. } => {
                let delta = if down { 3 } else { -3 };
                let last = self.visible_rows().len().saturating_sub(1);
                self.status.cursor = self.status.cursor.saturating_add_signed(delta).min(last);
            }
            // single-click selects; double-click activates (open file/commit,
            // or fold the section/dir/recent header) — like `<cr>`/`<tab>`
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
            Some(Row::File { .. } | Row::Commit { .. } | Row::CiRun { .. }) => {
                self.open_at_cursor();
            }
            Some(
                Row::SectionHeader { .. }
                | Row::Dir { .. }
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
                let last = self.visible_rows().len().saturating_sub(1);
                self.status.cursor = (self.status.cursor + 1).min(last);
            }
            Action::MoveUp => self.status.cursor = self.status.cursor.saturating_sub(1),
            Action::HalfPageDown => self.status_page(true, false),
            Action::HalfPageUp => self.status_page(false, false),
            Action::FullPageDown => self.status_page(true, true),
            Action::FullPageUp => self.status_page(false, true),
            Action::GoTop => self.status.cursor = 0,
            Action::GoBottom => {
                self.status.cursor = self.visible_rows().len().saturating_sub(1);
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
            Action::OpenReviewDiff => self.open_working_tree_diff(None),
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
            Action::Push => self.request_network(NetworkOp::Push, "push"),
            Action::PushSetUpstream => {
                self.request_network(NetworkOp::PushSetUpstream, "push -u");
            }
            Action::Pull => self.request_network(NetworkOp::Pull, "pull"),
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

    fn editor_at_status_cursor(&mut self) {
        let Some(row) = self.cursor_row() else {
            self.info("no file under the cursor");
            return;
        };
        // For an expanded inline diff line, pass the line number so the
        // editor opens at the right spot — same as the dedicated diff view.
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
            Row::Commit { index } => {
                let Some(oid) = self.status.recent.get(*index).map(|e| e.oid.clone()) else {
                    return;
                };
                self.open_commit_diff(&oid);
            }
            // a CI run opens its graph directly; the header opens the full Runs list
            Row::CiRun { index } => {
                self.runs_cursor = *index;
                self.open_selected_run();
            }
            Row::CiHeader { .. } => self.open_runs(),
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
                let position = self.visible_rows().iter().position(
                    |row| matches!(row, Row::Dir { section: s, path: p, .. } if *s == section && *p == path),
                );
                if let Some(position) = position {
                    self.status.cursor = position;
                }
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
                let position = self.visible_rows().iter().position(
                    |row| matches!(row, Row::File { section: s, index, .. } if *s == section && *index == file),
                );
                if let Some(position) = position {
                    self.status.cursor = position;
                }
            }
            Row::RecentHeader { .. } | Row::Commit { .. } => {
                self.status.recent_folded ^= true;
                let position = self
                    .visible_rows()
                    .iter()
                    .position(|row| matches!(row, Row::RecentHeader { .. }));
                if let Some(position) = position {
                    self.status.cursor = position;
                }
            }
            Row::CiHeader { .. } | Row::CiRun { .. } => {
                self.status.ci_folded ^= true;
                let position = self
                    .visible_rows()
                    .iter()
                    .position(|row| matches!(row, Row::CiHeader { .. }));
                if let Some(position) = position {
                    self.status.cursor = position;
                }
            }
        }
        self.clamp_cursor();
    }

    fn cursor_to_section_header(&mut self, section: Section) {
        let position = self
            .visible_rows()
            .iter()
            .position(|row| matches!(row, Row::SectionHeader { section: s, .. } if *s == section));
        if let Some(position) = position {
            self.status.cursor = position;
        }
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
            Row::RecentHeader { .. } => CursorAnchor::Recent,
            Row::Commit { index } => CursorAnchor::Commit(*index),
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
        self.status.cursor = self
            .status
            .cursor
            .min(self.visible_rows().len().saturating_sub(1));
    }
}

fn is_hunk_header(row: &Row) -> bool {
    matches!(row, Row::HunkHeader { .. })
}

fn is_section_header(row: &Row) -> bool {
    matches!(
        row,
        Row::SectionHeader { .. } | Row::RecentHeader { .. } | Row::CiHeader { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::super::{DiffSource, Screen};
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
        cursor_to(&mut app, |row| matches!(row, Row::CiRun { .. }));
        app.handle(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.screen(), crate::app::Screen::Graph);
        assert!(app.graph.is_some());
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let (_fixture, mut app) = app();
        // flat default: untracked (header + todo.md) + unstaged (header +
        // lib.rs) + staged (header + ci.yml) + recent commits header: 7 rows
        assert_eq!(app.visible_rows().len(), 7);
        app.handle(key('k'));
        assert_eq!(app.status.cursor, 0, "MoveUp clamps at the top");
        for _ in 0..20 {
            app.handle(key('j'));
        }
        assert_eq!(app.status.cursor, 6, "MoveDown clamps at the last row");
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
        assert_eq!(app.visible_rows().len(), 7);
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
        assert_eq!(app.status.cursor, 6);
        app.handle(ctrl_key('u'));
        assert_eq!(app.status.cursor, 0);
    }

    #[test]
    fn fold_toggles_the_section_under_the_cursor() {
        let (_fixture, mut app) = app();
        // the untracked section holds one root-level file (todo.md)
        app.handle(key('\t'));
        assert!(app.is_folded(Section::Untracked));
        assert_eq!(app.visible_rows().len(), 6);
        app.handle(key('\t'));
        assert!(!app.is_folded(Section::Untracked));
        assert_eq!(app.visible_rows().len(), 7);
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
                    Row::SectionHeader { .. } | Row::RecentHeader { .. } | Row::CiHeader { .. }
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
                    Row::SectionHeader { .. } | Row::RecentHeader { .. } | Row::CiHeader { .. }
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
        app.handle(key('}'));
        let first = app.status.cursor;
        assert!(is_hunk_header(&app.visible_rows()[first]));
        app.handle(key('}'));
        let second = app.status.cursor;
        assert!(second > first, "second hunk header is further down");
        assert!(is_hunk_header(&app.visible_rows()[second]));
        app.handle(key('{'));
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
            Row::RecentHeader { .. }
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
        app.handle(key('}'));
        assert!(is_hunk_header(&app.visible_rows()[app.status.cursor]));
        app.handle(key('s'));
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
        assert_eq!(app.section_files(Section::Untracked).len(), 0);
        assert_eq!(app.section_files(Section::Unstaged).len(), 0);
        assert_eq!(app.section_files(Section::Staged).len(), 3);
        app.handle(key('U'));
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
        assert_eq!(diff.source, DiffSource::WorkingTree);
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
        assert_eq!(diff.source, DiffSource::WorkingTree);
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
        assert_eq!(diff.source, DiffSource::WorkingTree);
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
        let DiffSource::Commit { oid } = &diff.source else {
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
        assert_eq!(app.section_files(Section::Untracked).len(), 2);
    }

    #[test]
    fn cursor_anchor_survives_refresh() {
        let (fixture, mut app) = app();
        cursor_to(&mut app, file_row_in(Section::Staged));
        // a new untracked file shifts every row below the untracked section
        fixture.write("aaa.md", "first\n");
        app.handle(ctrl_key('r'));
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
        assert_eq!(app.status.recent.len(), 2);
        assert_eq!(app.status.recent[0].subject, "second commit");
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
}
