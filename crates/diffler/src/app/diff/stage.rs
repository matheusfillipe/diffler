//! Staging from the working tree diff: `s` moves the selected lines, or the
//! hunk under the cursor, into the index, and takes them back out when the
//! index already holds them all.

use std::path::Path;

use diffler_core::model::{DiffLine, DiffModel, FileDiff, LineKind, LineSelection};
use diffler_core::source::ReviewSource;

use super::rows::DiffRow;
use crate::app::App;
use crate::app::rowsel::RowSelect;

impl App {
    /// The changed lines `s` acts on, with the file they belong to.
    fn stage_targets(&self) -> Result<(String, Vec<DiffLine>), &'static str> {
        const NOTHING: &str = "move onto a hunk, or select lines, to stage them";
        let diff = self.diff.as_ref().ok_or(NOTHING)?;
        let model_cow = diff.model_for_rows(&self.review);
        let model: &DiffModel = &model_cow;
        let rows: Vec<DiffRow> = match diff.selection() {
            Some((lo, hi)) => diff.rows.get(lo..=hi).unwrap_or_default().to_vec(),
            None => match diff.rows.get(diff.cursor) {
                Some(DiffRow::Hunk { file, hunk } | DiffRow::Line { file, hunk, .. }) => {
                    let lines = model
                        .files
                        .get(*file)
                        .and_then(|f| f.hunks.get(*hunk))
                        .map_or(0, |h| h.lines.len());
                    (0..lines)
                        .map(|line| DiffRow::Line {
                            file: *file,
                            hunk: *hunk,
                            line,
                        })
                        .collect()
                }
                _ => return Err(NOTHING),
            },
        };
        let mut owner: Option<usize> = None;
        let mut lines = Vec::new();
        for row in rows {
            let DiffRow::Line { file, hunk, line } = row else {
                continue;
            };
            if owner.is_some_and(|owner| owner != file) {
                return Err("select lines of one file to stage them");
            }
            owner = Some(file);
            let line = model
                .files
                .get(file)
                .and_then(|f| f.hunks.get(hunk))
                .and_then(|h| h.lines.get(line));
            if let Some(line) = line.filter(|line| line.kind != LineKind::Context) {
                lines.push(line.clone());
            }
        }
        let file = owner
            .and_then(|file| model.files.get(file))
            .ok_or(NOTHING)?;
        if file.old_path.is_some() {
            return Err("stage a renamed file whole, from the status screen");
        }
        if lines.is_empty() {
            return Err("select an added or deleted line to stage it");
        }
        Ok((file.path.clone(), lines))
    }

    /// Whether the diff on screen can be staged from; says why when not.
    fn staging_available(&mut self) -> bool {
        let on_working_tree = self
            .diff
            .as_ref()
            .is_some_and(|diff| matches!(diff.source, ReviewSource::WorkingTree));
        if !on_working_tree {
            self.info("open the working tree diff to stage changes");
            return false;
        }
        if !self.review.vcs.has_index() {
            self.info("commit the whole working copy; jj has no staging area");
            return false;
        }
        true
    }

    /// `s` in the file sidebar: stage the selected file, and unstage it once
    /// nothing of it is left to stage.
    pub(super) fn toggle_staged_selected_file(&mut self) {
        if !self.staging_available() {
            return;
        }
        let Some(path) = self
            .diff
            .as_ref()
            .and_then(|diff| diff.selected_path(&self.review))
        else {
            self.info("select a file to stage it");
            return;
        };
        let status = &self.review.status;
        let listed = |model: &DiffModel| model.files.iter().any(|f| f.path == path);
        let stage = listed(&status.unstaged) || listed(&status.untracked);
        self.vcs_op(move |vcs| {
            if stage {
                vcs.stage(Path::new(&path))
            } else {
                vcs.unstage(Path::new(&path))
            }
        });
    }

    /// `s`: stage what is not staged yet, and unstage once it all is.
    pub(super) fn toggle_staged_at_cursor(&mut self) {
        if !self.staging_available() {
            return;
        }
        let (path, lines) = match self.stage_targets() {
            Ok(targets) => targets,
            Err(reason) => {
                self.info(reason);
                return;
            }
        };
        let status = &self.review.status;
        let staged_lines = status.staged_lines();
        let (staged, unstaged): (Vec<&DiffLine>, Vec<&DiffLine>) = lines
            .iter()
            .partition(|line| staged_lines.is_staged(&path, line));
        let file_in = |model: &DiffModel| model.files.iter().find(|f| f.path == path).cloned();
        let head_to_index = file_in(&status.staged);
        let index_to_worktree = file_in(&status.unstaged).or_else(|| file_in(&status.untracked));
        let stage = !unstaged.is_empty();
        // We renumber index-side lines because the backend counts them in its own index diff.
        let selection = if stage {
            let mut selection =
                in_index_coordinates(&unstaged, LineKind::Deleted, head_to_index.as_ref(), true);
            if let (Some(index_to_worktree), Some(head_to_index)) =
                (index_to_worktree.as_ref(), head_to_index.as_ref())
            {
                take_replaced_staged_lines(&mut selection, index_to_worktree, head_to_index);
            }
            selection
        } else {
            in_index_coordinates(&staged, LineKind::Added, index_to_worktree.as_ref(), false)
        };
        if let Some(diff) = self.diff.as_mut() {
            diff.set_anchor(None);
        }
        self.vcs_op(move |vcs| {
            if stage {
                vcs.stage_lines(Path::new(&path), &selection)
            } else {
                vcs.unstage_lines(Path::new(&path), &selection)
            }
        });
    }
}

/// A staged line edited again shows in the working tree diff only as its
/// newest text, so we also drop the index's own copy from every hunk the
/// selection touches.
fn take_replaced_staged_lines(
    selection: &mut LineSelection,
    index_to_worktree: &FileDiff,
    head_to_index: &FileDiff,
) {
    for hunk in &index_to_worktree.hunks {
        if !hunk.lines.iter().any(|line| selection.contains(line)) {
            continue;
        }
        let staged_only = hunk
            .lines
            .iter()
            .filter(|line| line.kind == LineKind::Deleted)
            .filter_map(|line| line.old_no)
            .filter(|&number| head_to_index.counterpart_line(number, false).is_none())
            .collect::<Vec<_>>();
        selection.deleted.extend(staged_only);
    }
}

/// `lines` as a selection, with the lines of `through_kind` renumbered onto
/// the index through `through`, the diff between the index and the side those
/// lines were numbered on.
fn in_index_coordinates(
    lines: &[&DiffLine],
    through_kind: LineKind,
    through: Option<&FileDiff>,
    from_old: bool,
) -> LineSelection {
    let mut selection = LineSelection::default();
    for line in lines {
        if line.kind != through_kind {
            selection.insert(line);
            continue;
        }
        let on_index = line.number_on(from_old).and_then(|number| match through {
            Some(file) => file.counterpart_line(number, from_old),
            None => Some(number),
        });
        let Some(on_index) = on_index else {
            continue;
        };
        match through_kind {
            LineKind::Deleted => selection.deleted.insert(on_index),
            LineKind::Added | LineKind::Context => selection.added.insert(on_index),
        };
    }
    selection
}
