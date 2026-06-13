//! Status screen state and handlers: neogit-style sections with inline
//! diff expansion, folding, stage/unstage/discard, and cursor preservation
//! across refreshes.

use std::collections::BTreeSet;
use std::path::Path;

use diffler_core::model::FileDiff;
use diffler_core::vcs::LogEntry;

use super::{App, Modal, PendingOp};
use crate::keymap::Action;

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

/// One cursor-addressable row of the status screen: section headers, file
/// rows, and — when a file is expanded inline — hunk headers and diff lines,
/// plus the trailing Recent commits section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    SectionHeader {
        section: Section,
        count: usize,
    },
    File {
        section: Section,
        index: usize,
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
}

/// Where the cursor logically sits, so it can be restored after a refresh
/// reshuffles the rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CursorAnchor {
    Section(Section),
    File {
        section: Section,
        path: String,
        hunk: Option<usize>,
    },
    Recent,
    Commit(usize),
}

/// All state owned by the status screen.
pub struct StatusView {
    pub cursor: usize,
    pub folded: [bool; 3],
    pub recent: Vec<LogEntry>,
    pub recent_folded: bool,
    /// Per-section set of file paths whose inline diff is expanded.
    expanded: [BTreeSet<String>; 3],
}

impl StatusView {
    pub(super) fn new(recent: Vec<LogEntry>) -> Self {
        Self {
            cursor: 0,
            folded: [false; 3],
            recent,
            recent_folded: true,
            expanded: [const { BTreeSet::new() }; 3],
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
            for (index, file) in files.iter().enumerate() {
                rows.push(Row::File { section, index });
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
        if !self.status.recent.is_empty() {
            rows.push(Row::RecentHeader {
                count: self.status.recent.len(),
            });
            if !self.status.recent_folded {
                rows.extend((0..self.status.recent.len()).map(|index| Row::Commit { index }));
            }
        }
        rows
    }

    pub fn section_files(&self, section: Section) -> &[FileDiff] {
        let model = match section {
            Section::Untracked => &self.review.status.untracked,
            Section::Unstaged => &self.review.status.unstaged,
            Section::Staged => &self.review.status.staged,
        };
        &model.files
    }

    pub(super) fn dispatch_status(&mut self, action: Action) {
        match action {
            Action::MoveDown => {
                let last = self.visible_rows().len().saturating_sub(1);
                self.status.cursor = (self.status.cursor + 1).min(last);
            }
            Action::MoveUp => self.status.cursor = self.status.cursor.saturating_sub(1),
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
            Action::BranchPopup => self.open_branch_popup(),
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
        let Some(path) = self.row_file(row).map(|(_, file, _)| file.path.clone()) else {
            self.info("no file under the cursor");
            return;
        };
        self.request_editor(&path, line_no);
    }

    fn cursor_row(&self) -> Option<Row> {
        self.visible_rows().get(self.status.cursor).copied()
    }

    /// The file a row addresses, with the hunk index for hunk-scoped rows.
    pub fn row_file(&self, row: Row) -> Option<(Section, &FileDiff, Option<usize>)> {
        match row {
            Row::File { section, index } => self
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
            Row::SectionHeader { .. } | Row::RecentHeader { .. } | Row::Commit { .. } => None,
        }
    }

    fn stage_at_cursor(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        let Some((section, file, hunk)) = self.row_file(row) else {
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
        let Some((section, file, hunk)) = self.row_file(row) else {
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
        let Some((_, file, _)) = self.row_file(row) else {
            return;
        };
        let path = file.path.clone();
        self.modal = Some(Modal::Confirm {
            message: format!("Discard changes to {path}?"),
            on_confirm: PendingOp::Discard { path },
        });
    }

    fn open_at_cursor(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        match row {
            Row::Commit { index } => {
                let Some(oid) = self.status.recent.get(index).map(|e| e.oid.clone()) else {
                    return;
                };
                self.open_commit_diff(&oid);
            }
            // a section header opens the full review diff, starting the
            // walk at the section's first file (when the review covers it)
            Row::SectionHeader { section, .. } => {
                let path = self
                    .section_files(section)
                    .iter()
                    .find(|f| self.review.model.files.iter().any(|m| m.path == f.path))
                    .map(|f| f.path.clone());
                self.open_working_tree_diff(path.as_deref());
            }
            row => {
                let Some(path) = self.row_file(row).map(|(_, file, _)| file.path.clone()) else {
                    return;
                };
                self.open_working_tree_diff(Some(&path));
            }
        }
    }

    fn toggle_viewed(&mut self) {
        let Some(row) = self.cursor_row() else {
            return;
        };
        let Some(path) = self.row_file(row).map(|(_, file, _)| file.path.clone()) else {
            return;
        };
        let Some(hash) = self
            .review
            .model
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
            Row::File { section, index } => {
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
                    |row| matches!(row, Row::File { section: s, index } if *s == section && *index == file),
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
        Some(match row {
            Row::SectionHeader { section, .. } => CursorAnchor::Section(section),
            Row::RecentHeader { .. } => CursorAnchor::Recent,
            Row::Commit { index } => CursorAnchor::Commit(index),
            Row::File { .. } | Row::HunkHeader { .. } | Row::DiffLine { .. } => {
                let (section, file, hunk) = self.row_file(row)?;
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
            CursorAnchor::File {
                section,
                path,
                hunk,
            } => {
                let file_at = |row: &Row| -> Option<(Section, usize)> {
                    match row {
                        Row::File { section, index } => Some((*section, *index)),
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

    fn clamp_cursor(&mut self) {
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
    matches!(row, Row::SectionHeader { .. } | Row::RecentHeader { .. })
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

    /// Move the cursor onto the first row matching `pred`.
    fn cursor_to(app: &mut App, pred: impl Fn(&Row) -> bool) -> Row {
        let rows = app.visible_rows();
        let position = rows.iter().position(pred).expect("row present");
        app.status.cursor = position;
        rows[position]
    }

    fn file_row_in(section: Section) -> impl Fn(&Row) -> bool {
        move |row| matches!(row, Row::File { section: s, .. } if *s == section)
    }

    fn esc() -> AppEvent {
        AppEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let (_fixture, mut app) = app();
        // 3 sections of 1 file each + the recent commits header: 7 rows
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
    fn fold_toggles_the_section_under_the_cursor() {
        let (_fixture, mut app) = app();
        app.handle(key('\t'));
        assert!(app.is_folded(Section::Untracked));
        assert_eq!(app.visible_rows().len(), 6);
        app.handle(key('\t'));
        assert!(!app.is_folded(Section::Untracked));
        assert_eq!(app.visible_rows().len(), 7);
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
            app.diff_cursor_path().as_deref(),
            Some("ci.yml"),
            "cursor on the staged section's first review file"
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
        app.review.model.files.retain(|f| f.path != "ci.yml");
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
        let DiffSource::Commit(oid) = &diff.source else {
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
            .and_then(|row| app.row_file(*row))
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
        let row = app.visible_rows()[app.status.cursor];
        let (section, file, _) = app.row_file(row).expect("file row");
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
}
