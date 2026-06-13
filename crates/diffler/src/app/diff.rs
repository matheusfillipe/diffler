//! Diff/review screen state and handlers: a continuous scroll of file
//! headers, hunks, diff lines, and inline comments, flattened into a row
//! list so the renderer only ever materializes the visible slice.

use std::collections::{BTreeSet, HashMap};

use diffler_core::feedback::{self, FeedbackOptions};
use diffler_core::highlight::StyledRange;
use diffler_core::model::{DiffModel, FileDiff};
use diffler_core::review::Review;
use diffler_core::session::{Anchor, Comment, CommentStatus, Session};

use super::{App, InputOp, Modal, Screen};
use crate::clipboard;
use crate::keymap::Action;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSource {
    WorkingTree,
    Commit(String),
}

/// One terminal row of the flattened diff view. Indices point into the
/// model the view renders; the row list is rebuilt whenever folds, the
/// model, or the session change, so they never dangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
    File {
        file: usize,
    },
    Hunk {
        file: usize,
        hunk: usize,
    },
    Line {
        file: usize,
        hunk: usize,
        line: usize,
    },
    /// One display line of a comment block; `line` indexes the block
    /// produced by [`comment_display`].
    Comment {
        comment: usize,
        line: usize,
        outdated: bool,
    },
}

/// Per-line syntax spans for both sides of one file, keyed by the
/// both-sides content hash so edits to either side invalidate naturally.
/// Filled lazily by the renderer.
pub struct FileHighlights {
    pub hash: String,
    pub old: Vec<Vec<StyledRange>>,
    pub new: Vec<Vec<StyledRange>>,
}

pub struct DiffView {
    pub source: DiffSource,
    /// A commit's diff is immutable: fetched once at open and kept here.
    /// `None` means the view reads the live `review.model`.
    pub(crate) commit_model: Option<DiffModel>,
    pub cursor: usize,
    /// First visible row; the renderer keeps the cursor in view.
    pub scroll: usize,
    /// Row where `V` started; `Some` means line selection is active.
    pub visual_anchor: Option<usize>,
    /// Body height of the last render, drives half-page motions.
    pub(crate) viewport: u16,
    folded: BTreeSet<String>,
    pub(crate) rows: Vec<DiffRow>,
    rows_dirty: bool,
    pub(crate) highlights: HashMap<String, FileHighlights>,
}

impl DiffView {
    fn new(source: DiffSource, commit_model: Option<DiffModel>, review: &Review) -> Self {
        let mut folded = BTreeSet::new();
        if source == DiffSource::WorkingTree {
            // viewed files read as done: they start collapsed
            for file in &review.model.files {
                if review.session.is_viewed(&file.path, &file.content_hash()) {
                    folded.insert(file.path.clone());
                }
            }
        }
        let mut view = Self {
            source,
            commit_model,
            cursor: 0,
            scroll: 0,
            visual_anchor: None,
            viewport: 0,
            folded,
            rows: Vec::new(),
            rows_dirty: true,
            highlights: HashMap::new(),
        };
        view.ensure_rows(review);
        view
    }

    pub fn model<'a>(&'a self, review: &'a Review) -> &'a DiffModel {
        self.commit_model.as_ref().unwrap_or(&review.model)
    }

    pub fn rows(&self) -> &[DiffRow] {
        &self.rows
    }

    pub fn is_folded(&self, path: &str) -> bool {
        self.folded.contains(path)
    }

    /// Mark the row list stale. Selection is dropped with it: visual
    /// anchors are row indices and would dangle across a rebuild.
    pub(crate) fn invalidate(&mut self) {
        self.rows_dirty = true;
        self.visual_anchor = None;
    }

    pub(crate) fn ensure_rows(&mut self, review: &Review) {
        if !self.rows_dirty {
            return;
        }
        let model = self.commit_model.as_ref().unwrap_or(&review.model);
        let rows = build_rows(model, &review.session, &self.folded);
        self.rows = rows;
        self.rows_dirty = false;
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    /// Inclusive row span the visual selection covers, when active.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.visual_anchor?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// Row index of `path`'s file header.
    fn file_row(&self, model: &DiffModel, path: &str) -> Option<usize> {
        self.rows.iter().position(|row| {
            matches!(row, DiffRow::File { file }
                if model.files.get(*file).is_some_and(|f| f.path == path))
        })
    }
}

/// One display line of a comment block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentLine {
    Header,
    Body(String),
    Reply {
        author: String,
        text: String,
        first: bool,
    },
    Footer,
}

/// The terminal lines a comment occupies. Shared by row flattening (for
/// counts) and rendering (for content) so they can never disagree.
pub fn comment_display(comment: &Comment) -> Vec<CommentLine> {
    let mut lines = vec![CommentLine::Header];
    lines.extend(
        comment
            .body
            .lines()
            .map(|l| CommentLine::Body(l.to_owned())),
    );
    for reply in &comment.replies {
        let mut first = true;
        for text in reply.body.lines() {
            lines.push(CommentLine::Reply {
                author: reply.author.clone(),
                text: text.to_owned(),
                first,
            });
            first = false;
        }
    }
    lines.push(CommentLine::Footer);
    lines
}

/// Hunk and line indices a comment displays under; `None` when the
/// anchored line is absent from the file's hunks. Outdated detection lives
/// in [`Anchor::is_outdated`], which shares the same end-line semantics.
fn anchor_target(file: &FileDiff, anchor: &Anchor) -> Option<(usize, usize)> {
    // range comments display under the end of their range
    let target = anchor.line_end.or(anchor.line)?;
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        let found = hunk.lines.iter().position(|l| {
            let no = if anchor.on_old_side {
                l.old_no
            } else {
                l.new_no
            };
            no == Some(target)
        });
        if let Some(line_idx) = found {
            return Some((hunk_idx, line_idx));
        }
    }
    None
}

fn push_comment_rows(rows: &mut Vec<DiffRow>, session: &Session, comments: &[(usize, bool)]) {
    for &(comment, outdated) in comments {
        let Some(c) = session.comments.get(comment) else {
            continue;
        };
        let count = comment_display(c).len();
        rows.extend((0..count).map(|line| DiffRow::Comment {
            comment,
            line,
            outdated,
        }));
    }
}

fn build_rows(model: &DiffModel, session: &Session, folded: &BTreeSet<String>) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for (file_idx, file) in model.files.iter().enumerate() {
        rows.push(DiffRow::File { file: file_idx });
        if folded.contains(&file.path) {
            continue;
        }
        let mut by_line: HashMap<(usize, usize), Vec<(usize, bool)>> = HashMap::new();
        let mut unanchored: Vec<(usize, bool)> = Vec::new();
        for (comment_idx, comment) in session.comments.iter().enumerate() {
            if comment.anchor.file != file.path {
                continue;
            }
            let outdated = comment.anchor.is_outdated(model);
            match anchor_target(file, &comment.anchor) {
                Some((hunk, line)) => by_line
                    .entry((hunk, line))
                    .or_default()
                    .push((comment_idx, outdated)),
                // a line anchor that no longer exists is outdated; a
                // file-level comment (no line) simply lives under the header
                None => unanchored.push((comment_idx, outdated)),
            }
        }
        push_comment_rows(&mut rows, session, &unanchored);
        for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
            rows.push(DiffRow::Hunk {
                file: file_idx,
                hunk: hunk_idx,
            });
            for line_idx in 0..hunk.lines.len() {
                rows.push(DiffRow::Line {
                    file: file_idx,
                    hunk: hunk_idx,
                    line: line_idx,
                });
                if let Some(list) = by_line.get(&(hunk_idx, line_idx)) {
                    push_comment_rows(&mut rows, session, list);
                }
            }
        }
    }
    rows
}

impl App {
    pub(crate) fn open_working_tree_diff(&mut self, scope: Option<&str>) {
        let mut view = DiffView::new(DiffSource::WorkingTree, None, &self.review);
        if let Some(path) = scope
            && let Some(position) = view.file_row(&self.review.model, path)
        {
            view.cursor = position;
            view.scroll = position;
        }
        self.diff = Some(view);
        self.screens.push(Screen::Diff);
    }

    pub(crate) fn open_commit_diff(&mut self, oid: &str) {
        match self.review.vcs.commit_diff(oid) {
            Ok(model) => {
                let view = DiffView::new(
                    DiffSource::Commit(oid.to_owned()),
                    Some(model),
                    &self.review,
                );
                self.diff = Some(view);
                self.screens.push(Screen::Diff);
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    pub(super) fn dispatch_diff(&mut self, action: Action) {
        if let Some(diff) = self.diff.as_mut() {
            diff.ensure_rows(&self.review);
        } else {
            return;
        }
        match action {
            Action::MoveDown => self.diff_move(1),
            Action::MoveUp => self.diff_move(-1),
            Action::GoTop => self.diff_move(isize::MIN),
            Action::GoBottom => self.diff_move(isize::MAX),
            Action::HalfPageDown => self.diff_move(self.diff_half_page()),
            Action::HalfPageUp => self.diff_move(-self.diff_half_page()),
            Action::NextHunk => self.diff_jump(true, |row| matches!(row, DiffRow::Hunk { .. })),
            Action::PrevHunk => self.diff_jump(false, |row| matches!(row, DiffRow::Hunk { .. })),
            Action::NextSection => self.diff_jump(true, |row| matches!(row, DiffRow::File { .. })),
            Action::PrevSection => self.diff_jump(false, |row| matches!(row, DiffRow::File { .. })),
            Action::ToggleFold => self.diff_toggle_fold(),
            Action::Comment => self.comment_at_cursor(),
            Action::VisualSelect => self.toggle_visual(),
            Action::Reply => self.reply_at_cursor(),
            Action::Resolve => self.resolve_at_cursor(),
            Action::MarkViewed => self.diff_toggle_viewed(),
            Action::CopyFileFeedback => self.copy_feedback(true),
            Action::CopyAllFeedback => self.copy_feedback(false),
            Action::OpenEditor => self.editor_at_diff_cursor(),
            other => {
                self.info(format!("{} is not implemented yet", other.name()));
            }
        }
    }

    /// `e`: open the cursor's file in the editor — at the line for diff
    /// line rows (new side, old side for deletions), at the anchor for
    /// comment rows, at the top for file and hunk headers.
    fn editor_at_diff_cursor(&mut self) {
        let target = self.diff.as_ref().and_then(|diff| {
            let model = diff.model(&self.review);
            match diff.rows.get(diff.cursor)? {
                DiffRow::File { file } | DiffRow::Hunk { file, .. } => {
                    model.files.get(*file).map(|f| (f.path.clone(), None))
                }
                DiffRow::Line { file, hunk, line } => model.files.get(*file).and_then(|f| {
                    let line = f.hunks.get(*hunk)?.lines.get(*line)?;
                    Some((f.path.clone(), line.new_no.or(line.old_no)))
                }),
                DiffRow::Comment { comment, .. } => self
                    .review
                    .session
                    .comments
                    .get(*comment)
                    .map(|c| (c.anchor.file.clone(), c.anchor.line_end.or(c.anchor.line))),
            }
        });
        match target {
            Some((path, line)) => self.request_editor(&path, line),
            None => self.info("no file under the cursor"),
        }
    }

    fn diff_move(&mut self, delta: isize) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let last = diff.rows.len().saturating_sub(1);
        diff.cursor = diff.cursor.saturating_add_signed(delta).min(last);
    }

    fn diff_half_page(&self) -> isize {
        let viewport = self.diff.as_ref().map_or(0, |d| d.viewport);
        // before the first render the height is unknown; half a typical
        // terminal is a fine guess
        let half = if viewport == 0 {
            20
        } else {
            i64::from(viewport) / 2
        };
        isize::try_from(half).unwrap_or(20).max(1)
    }

    fn diff_jump(&mut self, forward: bool, target: impl Fn(&DiffRow) -> bool) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let position = if forward {
            diff.rows
                .iter()
                .enumerate()
                .skip(diff.cursor + 1)
                .find(|(_, row)| target(row))
                .map(|(index, _)| index)
        } else {
            diff.rows
                .iter()
                .enumerate()
                .take(diff.cursor)
                .rfind(|(_, row)| target(row))
                .map(|(index, _)| index)
        };
        if let Some(position) = position {
            diff.cursor = position;
        }
    }

    /// File path the cursor row belongs to.
    pub(crate) fn diff_cursor_path(&self) -> Option<String> {
        let diff = self.diff.as_ref()?;
        let model = diff.model(&self.review);
        match diff.rows.get(diff.cursor)? {
            DiffRow::File { file } | DiffRow::Hunk { file, .. } | DiffRow::Line { file, .. } => {
                model.files.get(*file).map(|f| f.path.clone())
            }
            DiffRow::Comment { comment, .. } => self
                .review
                .session
                .comments
                .get(*comment)
                .map(|c| c.anchor.file.clone()),
        }
    }

    /// After a refresh, follow the file the cursor was on if its row moved.
    pub(super) fn restore_diff_cursor(&mut self, path: Option<String>) {
        let Some(path) = path else {
            return;
        };
        if self.diff_cursor_path().as_deref() == Some(path.as_str()) {
            return;
        }
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let model = diff.commit_model.as_ref().unwrap_or(&self.review.model);
        if let Some(position) = diff.file_row(model, &path) {
            diff.cursor = position;
        }
    }

    fn diff_toggle_fold(&mut self) {
        let Some(path) = self.diff_cursor_path() else {
            return;
        };
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if !diff.folded.remove(&path) {
            diff.folded.insert(path.clone());
        }
        diff.invalidate();
        diff.ensure_rows(&self.review);
        self.diff_cursor_to_file(&path);
    }

    fn diff_cursor_to_file(&mut self, path: &str) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let model = diff.commit_model.as_ref().unwrap_or(&self.review.model);
        if let Some(position) = diff.file_row(model, path) {
            diff.cursor = position;
        }
    }

    fn toggle_visual(&mut self) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if diff.visual_anchor.take().is_some() {
            return;
        }
        if matches!(diff.rows.get(diff.cursor), Some(DiffRow::Line { .. })) {
            diff.visual_anchor = Some(diff.cursor);
        } else {
            self.info("move to a diff line to start a selection");
        }
    }

    /// Anchor for a new comment at the cursor (or the visual selection).
    fn comment_anchor(&self) -> Option<Anchor> {
        let diff = self.diff.as_ref()?;
        let model = diff.model(&self.review);
        let line_at = |row: &DiffRow| -> Option<(
            usize,
            &diffler_core::model::Hunk,
            &diffler_core::model::DiffLine,
        )> {
            let DiffRow::Line { file, hunk, line } = row else {
                return None;
            };
            let hunk_data = model.files.get(*file)?.hunks.get(*hunk)?;
            Some((*file, hunk_data, hunk_data.lines.get(*line)?))
        };
        let anchor_row = diff.visual_anchor.unwrap_or(diff.cursor);
        let (file_idx, hunk, line) = line_at(diff.rows.get(anchor_row)?)?;
        let file = model.files.get(file_idx)?;
        // deletions only exist on the old side; everything else anchors to
        // the new-side line number
        let on_old_side = line.new_no.is_none();
        let number = |l: &diffler_core::model::DiffLine| {
            if on_old_side { l.old_no } else { l.new_no }
        };

        let Some((start, end)) = diff.selection() else {
            return Some(Anchor {
                file: file.path.clone(),
                line: Some(number(line)?),
                line_end: None,
                on_old_side,
                hunk: Some(hunk.id.clone()),
                line_text: Some(line.text.clone()),
            });
        };
        // visual range: gather the selected line numbers on the anchor
        // line's side, restricted to the anchor's file
        let mut numbered: Vec<(u32, String)> = Vec::new();
        for index in start..=end {
            let Some(row) = diff.rows.get(index) else {
                continue;
            };
            if !matches!(row, DiffRow::Line { file, .. } if *file == file_idx) {
                continue;
            }
            let Some((_, _, l)) = line_at(row) else {
                continue;
            };
            if let Some(no) = number(l) {
                numbered.push((no, l.text.clone()));
            }
        }
        let (first, _) = numbered.iter().min_by_key(|(no, _)| *no)?.clone();
        let (last, last_text) = numbered.iter().max_by_key(|(no, _)| *no)?.clone();
        Some(Anchor {
            file: file.path.clone(),
            line: Some(first),
            line_end: (last > first).then_some(last),
            on_old_side,
            hunk: Some(hunk.id.clone()),
            // the display target is the range end, so that is the line
            // whose drift marks the comment outdated
            line_text: Some(last_text),
        })
    }

    fn comment_at_cursor(&mut self) {
        let Some(anchor) = self.comment_anchor() else {
            self.info("move to a diff line to comment");
            return;
        };
        let title = match (anchor.line, anchor.line_end) {
            (Some(line), Some(end)) => format!("Comment {}:{line}-{end}", anchor.file),
            (Some(line), None) => format!("Comment {}:{line}", anchor.file),
            _ => format!("Comment {}", anchor.file),
        };
        self.modal = Some(Modal::Input {
            title,
            buffer: String::new(),
            cursor: 0,
            on_submit: InputOp::Comment { anchor },
        });
    }

    fn comment_at_cursor_row(&self) -> Option<&Comment> {
        let diff = self.diff.as_ref()?;
        let DiffRow::Comment { comment, .. } = diff.rows.get(diff.cursor)? else {
            return None;
        };
        self.review.session.comments.get(*comment)
    }

    fn reply_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to reply");
            return;
        };
        let comment_id = comment.id.clone();
        let author = comment.author.clone();
        self.modal = Some(Modal::Input {
            title: format!("Reply to {author}"),
            buffer: String::new(),
            cursor: 0,
            on_submit: InputOp::Reply { comment_id },
        });
    }

    fn resolve_at_cursor(&mut self) {
        let Some(comment) = self.comment_at_cursor_row() else {
            self.info("move onto a comment to resolve");
            return;
        };
        if comment.status == CommentStatus::Resolved {
            self.info("already resolved");
            return;
        }
        let id = comment.id.clone();
        self.review.session.resolve(&id);
        self.after_session_change();
        self.info("comment resolved");
    }

    fn diff_toggle_viewed(&mut self) {
        let Some(path) = self.diff_cursor_path() else {
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
        let viewed = self.review.session.is_viewed(&path, &hash);
        if viewed {
            self.review.session.unmark_viewed(&path);
        } else {
            self.review.session.mark_viewed(&path, &hash);
        }
        if let Err(err) = self.review.save() {
            self.error(err.to_string());
        }
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        // marking viewed folds the file away; unmarking reopens it for
        // re-review
        if viewed {
            diff.folded.remove(&path);
        } else {
            diff.folded.insert(path.clone());
        }
        diff.invalidate();
        diff.ensure_rows(&self.review);
        self.diff_cursor_to_file(&path);
        // pressing v repeatedly walks the review: after marking, jump to
        // the next file still waiting to be looked at
        if !viewed {
            self.diff_advance_to_unviewed();
        }
    }

    /// Move the cursor to the next not-viewed file header below it, if any.
    fn diff_advance_to_unviewed(&mut self) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let model = diff.model(&self.review);
        let position =
            diff.rows
                .iter()
                .enumerate()
                .skip(diff.cursor + 1)
                .find_map(|(index, row)| {
                    let DiffRow::File { file } = row else {
                        return None;
                    };
                    let file = model.files.get(*file)?;
                    (!self.is_path_viewed(&file.path)).then_some(index)
                });
        if let (Some(position), Some(diff)) = (position, self.diff.as_mut()) {
            diff.cursor = position;
        }
    }

    fn copy_feedback(&mut self, file_only: bool) {
        let filter = if file_only {
            let Some(path) = self.diff_cursor_path() else {
                self.info("no file under the cursor");
                return;
            };
            Some(path)
        } else {
            None
        };
        let session = &self.review.session;
        let count = session
            .comments
            .iter()
            .filter(|c| c.status != CommentStatus::Resolved)
            .filter(|c| filter.as_deref().is_none_or(|f| c.anchor.file == f))
            .count();
        let noun = if count == 1 { "comment" } else { "comments" };
        if count == 0 {
            self.info("no comments to copy");
            return;
        }
        let repo = self
            .review
            .repo_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let branch = self.head.branch.clone().unwrap_or_else(|| "?".to_owned());
        let title = format!("Review feedback — {repo} @ {branch} ({count} {noun})");
        let markdown = feedback::to_markdown(
            session,
            &self.review.model,
            &FeedbackOptions {
                title: &title,
                file_filter: filter.as_deref(),
                include_resolved: false,
            },
        );
        self.pending_osc = Some(clipboard::osc52(&markdown));
        let scope = if file_only { "file" } else { "all" };
        self.info(format!("copied {count} {noun} ({scope})"));
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::session::CommentStatus;

    use super::*;
    use crate::app::Screen;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, ctrl_key, key, standard_fixture, two_hunk_fixture};

    fn diff_app(fixture: &Fixture) -> App {
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        app
    }

    fn rows(app: &App) -> Vec<DiffRow> {
        app.diff.as_ref().expect("diff view").rows().to_vec()
    }

    fn cursor_to_line(app: &mut App, pred: impl Fn(&DiffRow) -> bool) {
        let position = rows(app).iter().position(pred).expect("row present");
        app.diff.as_mut().unwrap().cursor = position;
    }

    /// Row index of the first added line ("42") in the standard fixture's
    /// src/lib.rs diff.
    fn added_line_position(app: &App) -> usize {
        let diff = app.diff.as_ref().unwrap();
        let model = diff.model(&app.review);
        rows(app)
            .iter()
            .position(|row| {
                let DiffRow::Line { file, hunk, line } = row else {
                    return false;
                };
                model.files.get(*file).is_some_and(|f| {
                    f.path == "src/lib.rs"
                        && f.hunks.get(*hunk).is_some_and(|h| {
                            h.lines
                                .get(*line)
                                .is_some_and(|l| l.new_no.is_some() && l.text.contains("42"))
                        })
                })
            })
            .expect("added line present")
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle(key(c));
        }
    }

    #[test]
    fn rows_flatten_files_hunks_and_lines_in_order() {
        let fixture = two_hunk_fixture();
        let app = diff_app(&fixture);
        let rows = rows(&app);
        assert!(matches!(rows.first(), Some(DiffRow::File { file: 0 })));
        let hunks: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                DiffRow::Hunk { hunk, .. } => Some(*hunk),
                _ => None,
            })
            .collect();
        assert_eq!(hunks, vec![0, 1], "both hunks flattened in order");
        // every line row sits after its hunk header
        assert!(
            rows.iter()
                .any(|r| matches!(r, DiffRow::Line { hunk: 1, .. })),
            "second hunk has line rows"
        );
    }

    #[test]
    fn comment_rows_appear_under_their_anchored_line() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('c'));
        assert!(matches!(app.modal, Some(Modal::Input { .. })));
        type_text(&mut app, "why 42?");
        app.handle(key('\n'));
        assert_eq!(app.modal, None);

        let comment = &app.review.session.comments[0];
        assert_eq!(comment.author, "reviewer");
        assert_eq!(comment.body, "why 42?");
        assert_eq!(comment.anchor.file, "src/lib.rs");
        assert_eq!(comment.anchor.line, Some(2));
        assert!(!comment.anchor.on_old_side);
        assert!(comment.anchor.hunk.is_some(), "anchor carries the hunk id");
        assert_eq!(comment.anchor.line_text.as_deref(), Some("    42"));

        // rows were invalidated: the comment block follows the anchored line
        let diff = app.diff.as_mut().unwrap();
        diff.ensure_rows(&app.review);
        let rows = rows(&app);
        let line_position = added_line_position(&app);
        let block: Vec<_> = rows
            .iter()
            .skip(line_position + 1)
            .take_while(|r| matches!(r, DiffRow::Comment { .. }))
            .collect();
        // header + one body line + footer
        assert_eq!(
            block.len(),
            3,
            "comment block right under the line: {rows:?}"
        );
        assert!(block.iter().all(|r| matches!(
            r,
            DiffRow::Comment {
                outdated: false,
                ..
            }
        )));
    }

    #[test]
    fn outdated_comment_is_flagged_when_the_line_text_drifts() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.review.session.add_comment(
            "reviewer",
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: false,
                hunk: None,
                line_text: Some("    43".to_owned()),
            },
            "stale snapshot",
        );
        let diff = app.diff.as_mut().unwrap();
        diff.invalidate();
        diff.ensure_rows(&app.review);
        assert!(
            rows(&app)
                .iter()
                .any(|r| matches!(r, DiffRow::Comment { outdated: true, .. })),
            "drifted line_text flags the comment outdated"
        );
    }

    #[test]
    fn comment_for_a_departed_line_attaches_under_the_file_header() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.review.session.add_comment(
            "reviewer",
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(99),
                line_end: None,
                on_old_side: false,
                hunk: None,
                line_text: None,
            },
            "moved on",
        );
        let diff = app.diff.as_mut().unwrap();
        diff.invalidate();
        diff.ensure_rows(&app.review);
        let rows = rows(&app);
        let model = app.diff.as_ref().unwrap().model(&app.review);
        let header = rows
            .iter()
            .position(|r| {
                matches!(r, DiffRow::File { file }
                    if model.files.get(*file).is_some_and(|f| f.path == "src/lib.rs"))
            })
            .expect("file header");
        assert!(
            matches!(
                rows.get(header + 1),
                Some(DiffRow::Comment { outdated: true, .. })
            ),
            "orphaned comment sits under the header, flagged outdated: {rows:?}"
        );
    }

    #[test]
    fn folded_file_collapses_to_its_header() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        let before = rows(&app).len();
        assert!(before > 1);
        app.handle(key('j'));
        app.handle(key('\t'));
        let rows = rows(&app);
        assert_eq!(rows.len(), 1, "only the file header remains");
        assert!(matches!(rows[0], DiffRow::File { .. }));
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0, "cursor on the header");
        app.handle(key('\t'));
        assert_eq!(self::rows(&app).len(), before);
    }

    #[test]
    fn scoped_open_starts_at_the_files_header() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(Some("src/lib.rs"));
        let diff = app.diff.as_ref().unwrap();
        let model = diff.model(&app.review);
        let DiffRow::File { file } = diff.rows()[diff.cursor] else {
            panic!("cursor must sit on a file header");
        };
        assert_eq!(model.files[file].path, "src/lib.rs");
        assert_eq!(diff.scroll, diff.cursor, "view starts scrolled to the file");
    }

    #[test]
    fn visual_selection_comments_a_new_side_range() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        // first hunk: "-line 1" then "+line one" then context lines
        cursor_to_line(&mut app, |r| {
            matches!(
                r,
                DiffRow::Line {
                    hunk: 0,
                    line: 1,
                    ..
                }
            )
        });
        app.handle(key('V'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(key('c'));
        type_text(&mut app, "this block");
        app.handle(key('\n'));
        let comment = &app.review.session.comments[0];
        assert_eq!(comment.anchor.line, Some(1));
        assert_eq!(comment.anchor.line_end, Some(3));
        assert!(!comment.anchor.on_old_side);
        assert_eq!(comment.anchor.line_text.as_deref(), Some("line 3"));
        assert!(
            app.diff.as_ref().unwrap().visual_anchor.is_none(),
            "selection ends once the comment lands"
        );
    }

    #[test]
    fn visual_selection_anchored_on_a_deleted_line_uses_the_old_side() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        // hunk 0 line 0 is the deleted "line 1"
        cursor_to_line(&mut app, |r| {
            matches!(
                r,
                DiffRow::Line {
                    hunk: 0,
                    line: 0,
                    ..
                }
            )
        });
        app.handle(key('V'));
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(key('c'));
        type_text(&mut app, "old side");
        app.handle(key('\n'));
        let comment = &app.review.session.comments[0];
        assert!(comment.anchor.on_old_side);
        // selected rows: -line 1 (old 1), +line one (no old no), context
        // line 2 (old 2) → range 1..2 on the old side
        assert_eq!(comment.anchor.line, Some(1));
        assert_eq!(comment.anchor.line_end, Some(2));
    }

    #[test]
    fn escape_cancels_visual_selection() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
        app.handle(key('V'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
        app.handle(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        )));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_none());
        // V twice toggles off as well
        app.handle(key('V'));
        app.handle(key('V'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_none());
    }

    #[test]
    fn reply_and_resolve_walk_the_comment_lifecycle() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('c'));
        type_text(&mut app, "question");
        app.handle(key('\n'));

        // the comment header row sits right under the anchored line
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position + 1;
        app.handle(key('r'));
        assert!(matches!(app.modal, Some(Modal::Input { .. })));
        type_text(&mut app, "answer");
        app.handle(key('\n'));
        let comment = &app.review.session.comments[0];
        assert_eq!(comment.status, CommentStatus::Replied);
        assert_eq!(comment.replies.len(), 1);
        assert_eq!(comment.replies[0].body, "answer");

        // the block grew by the reply line; resolve from the same header
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position + 1;
        app.handle(key('R'));
        assert_eq!(
            app.review.session.comments[0].status,
            CommentStatus::Resolved
        );
        // persisted to disk
        let reloaded = diffler_core::store::load(&fixture.root).unwrap();
        assert_eq!(reloaded.comments[0].status, CommentStatus::Resolved);
    }

    #[test]
    fn reply_off_a_comment_row_hints() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.handle(key('r'));
        let message = app.message.expect("message");
        assert!(message.text.contains("comment"));
    }

    #[test]
    fn viewed_toggle_folds_the_file_and_persists() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
        let path = app.diff_cursor_path().expect("path");
        app.handle(key('v'));
        assert!(app.is_path_viewed(&path));
        assert!(app.diff.as_ref().unwrap().is_folded(&path));
        let reloaded = diffler_core::store::load(&fixture.root).unwrap();
        assert!(reloaded.viewed.contains_key(&path));
        // the walk advanced to the next unviewed file's header
        let DiffRow::File { .. } = rows(&app)[app.diff.as_ref().unwrap().cursor] else {
            panic!("cursor should sit on a file header");
        };
        // step back onto the folded file and unmark it
        app.handle(key('k'));
        app.handle(key('v'));
        assert!(!app.is_path_viewed(&path));
        assert!(!app.diff.as_ref().unwrap().is_folded(&path));
    }

    #[test]
    fn marking_viewed_advances_to_the_next_unviewed_file() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let paths: Vec<String> = app
            .review
            .model
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect();
        assert_eq!(paths.len(), 3, "walk needs several files: {paths:?}");

        // v on the first file folds it and lands on the second's header
        app.handle(key('v'));
        assert!(app.is_path_viewed(&paths[0]));
        assert_eq!(app.diff_cursor_path().as_deref(), Some(paths[1].as_str()));
        let diff = app.diff.as_ref().unwrap();
        assert!(
            matches!(diff.rows()[diff.cursor], DiffRow::File { .. }),
            "cursor sits on the next file header"
        );

        // v-v walks the rest; the last v has nothing below and stays put
        app.handle(key('v'));
        assert_eq!(app.diff_cursor_path().as_deref(), Some(paths[2].as_str()));
        app.handle(key('v'));
        assert!(paths.iter().all(|p| app.is_path_viewed(p)));
        assert_eq!(
            app.diff_cursor_path().as_deref(),
            Some(paths[2].as_str()),
            "no unviewed file below: the cursor stays"
        );
    }

    #[test]
    fn unmarking_viewed_does_not_move_the_cursor() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let first = app.diff_cursor_path().expect("path");
        app.handle(key('v'));
        // back onto the folded file, then unmark it
        app.handle(key('k'));
        assert_eq!(app.diff_cursor_path().as_deref(), Some(first.as_str()));
        app.handle(key('v'));
        assert!(!app.is_path_viewed(&first));
        assert_eq!(
            app.diff_cursor_path().as_deref(),
            Some(first.as_str()),
            "unmarking reopens the file in place"
        );
    }

    #[test]
    fn viewed_files_start_folded_when_the_diff_opens() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let hash = app
            .review
            .model
            .files
            .iter()
            .find(|f| f.path == "src/lib.rs")
            .map(FileDiff::content_hash)
            .unwrap();
        app.review.session.mark_viewed("src/lib.rs", &hash);
        app.open_working_tree_diff(None);
        assert!(app.diff.as_ref().unwrap().is_folded("src/lib.rs"));
    }

    #[test]
    fn y_copies_the_current_files_feedback_as_osc52_markdown() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('c'));
        type_text(&mut app, "why 42?");
        app.handle(key('\n'));
        app.review.session.add_comment(
            "reviewer",
            Anchor {
                file: "todo.md".to_owned(),
                line: None,
                line_end: None,
                on_old_side: false,
                hunk: None,
                line_text: None,
            },
            "other file",
        );

        app.diff.as_mut().unwrap().cursor = added_line_position(&app);
        app.handle(key('y'));
        let payload = app.pending_osc.clone().expect("osc52 payload queued");
        let toast = app.message.clone().expect("toast");
        assert_eq!(toast.text, "copied 1 comment (file)");
        let repo = fixture.root.file_name().unwrap().to_string_lossy();
        let expected = feedback::to_markdown(
            &app.review.session,
            &app.review.model,
            &FeedbackOptions {
                title: &format!("Review feedback — {repo} @ main (1 comment)"),
                file_filter: Some("src/lib.rs"),
                include_resolved: false,
            },
        );
        assert!(expected.contains("why 42?"));
        assert!(!expected.contains("other file"), "file filter applies");
        assert_eq!(payload, clipboard::osc52(&expected));

        app.handle(key('Y'));
        let message = app.message.clone().expect("toast");
        assert_eq!(message.text, "copied 2 comments (all)");
    }

    #[test]
    fn y_with_no_comments_hints_instead_of_copying() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.handle(key('y'));
        assert_eq!(app.pending_osc, None);
        let message = app.message.expect("message");
        assert!(message.text.contains("no comments"));
    }

    #[test]
    fn e_on_a_diff_line_opens_the_editor_at_that_line() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        // pin the editor through config so the test ignores $EDITOR
        app.config.editor.command = Some("vim".to_owned());
        let position = added_line_position(&app);
        app.diff.as_mut().unwrap().cursor = position;
        app.handle(key('e'));
        let request = app.pending_editor.clone().expect("editor request");
        assert_eq!(
            request.purpose,
            crate::editor::EditorPurpose::OpenFile {
                path: "src/lib.rs".to_owned(),
            }
        );
        // the "42" line is line 2 on the new side
        let absolute = fixture.root.join("src/lib.rs");
        assert_eq!(
            request.cmd,
            vec![
                "vim".to_owned(),
                "+2".to_owned(),
                absolute.to_string_lossy().into_owned(),
            ]
        );
    }

    #[test]
    fn e_on_a_file_header_opens_without_a_line() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        app.config.editor.command = Some("vim".to_owned());
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::File { .. }));
        app.handle(key('e'));
        let request = app.pending_editor.clone().expect("editor request");
        assert!(
            request.cmd.iter().all(|arg| !arg.starts_with('+')),
            "no line jump from a header: {:?}",
            request.cmd
        );
    }

    #[test]
    fn half_page_motions_move_by_the_viewport() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        app.diff.as_mut().unwrap().viewport = 10;
        app.handle(ctrl_key('d'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, 5);
        app.handle(ctrl_key('u'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, 0);
    }

    #[test]
    fn hunk_and_file_jumps_move_between_headers() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        app.handle(key('}'));
        let first = app.diff.as_ref().unwrap().cursor;
        assert!(matches!(rows(&app)[first], DiffRow::Hunk { hunk: 0, .. }));
        app.handle(key('}'));
        assert!(matches!(
            rows(&app)[app.diff.as_ref().unwrap().cursor],
            DiffRow::Hunk { hunk: 1, .. }
        ));
        app.handle(key('{'));
        assert_eq!(app.diff.as_ref().unwrap().cursor, first);
    }

    #[test]
    fn noop_refresh_preserves_the_visual_selection() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
        app.handle(key('V'));
        assert!(app.diff.as_ref().unwrap().visual_anchor.is_some());
        // nothing changed on disk: a watcher echo or poll tick refresh
        // must not kill the selection
        app.handle(AppEvent::RepoChanged);
        assert!(
            app.diff.as_ref().unwrap().visual_anchor.is_some(),
            "no-op refresh keeps the selection"
        );
    }

    #[test]
    fn real_change_refresh_clears_the_visual_selection() {
        let fixture = two_hunk_fixture();
        let mut app = diff_app(&fixture);
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
        app.handle(key('V'));
        fixture.write("zzz.md", "new\n");
        app.handle(AppEvent::RepoChanged);
        assert!(
            app.diff.as_ref().unwrap().visual_anchor.is_none(),
            "rows shifted: a stale anchor would dangle"
        );
    }

    #[test]
    fn refresh_rebuilds_rows_and_follows_the_cursor_file() {
        let fixture = standard_fixture();
        let mut app = diff_app(&fixture);
        cursor_to_line(&mut app, |r| matches!(r, DiffRow::Line { .. }));
        let path = app.diff_cursor_path().expect("path");
        // a new file ahead of src/ shifts every row
        fixture.write("aaa.rs", "fn nope() {}\n");
        app.handle(ctrl_key('r'));
        assert_eq!(
            app.diff_cursor_path().as_deref(),
            Some(path.as_str()),
            "cursor follows its file across the refresh"
        );
    }

    #[test]
    fn commit_diff_survives_refresh_untouched() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let oid = app.status.recent[0].oid.clone();
        app.open_commit_diff(&oid);
        assert_eq!(app.screen(), Screen::Diff);
        let before = rows(&app).len();
        fixture.write("zzz.md", "new\n");
        app.handle(ctrl_key('r'));
        assert_eq!(rows(&app).len(), before, "commit model is immutable");
    }

    #[test]
    fn comment_display_lines_cover_body_and_replies() {
        let mut session = Session::default();
        let id = session
            .add_comment(
                "reviewer",
                Anchor {
                    file: "a.rs".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    hunk: None,
                    line_text: None,
                },
                "first\nsecond",
            )
            .id
            .clone();
        session.reply(&id, "agent", "done\nand verified");
        let lines = comment_display(&session.comments[0]);
        assert_eq!(
            lines,
            vec![
                CommentLine::Header,
                CommentLine::Body("first".to_owned()),
                CommentLine::Body("second".to_owned()),
                CommentLine::Reply {
                    author: "agent".to_owned(),
                    text: "done".to_owned(),
                    first: true,
                },
                CommentLine::Reply {
                    author: "agent".to_owned(),
                    text: "and verified".to_owned(),
                    first: false,
                },
                CommentLine::Footer,
            ]
        );
    }
}
