//! Flattening a file's diff and its review comments into the row list the
//! pane renders.

use std::collections::HashMap;

use diffler_core::highlight::Highlighter;
use diffler_core::model::{DiffModel, FileDiff, LineKind};
use diffler_core::session::{Anchor, Comment, Session};
use unicode_width::UnicodeWidthStr;

use crate::app::composer::{Composer, ComposerKind, card_budget};
use crate::app::markdown::{self, MdSpan};

/// One terminal row of the selected file's diff body. Indices point into the
/// model the view renders; the row list is rebuilt whenever the selected
/// file, the model, or the session change, so they never dangle. The `file`
/// field always equals the selected file index, kept so the shared
/// `diff_render` and anchor helpers read it unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRow {
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
    /// One display line of the open composer, sitting where its result will.
    Composer {
        line: usize,
    },
}

/// One display line of a comment block. Body and reply text carry markdown
/// styling as flag-tagged runs; [`crate::ui`] maps the flags to concrete styles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentLine {
    Header,
    Body(Vec<MdSpan>),
    Reply {
        author: String,
        spans: Vec<MdSpan>,
        first: bool,
    },
    Footer,
}

/// The terminal lines a comment occupies at `row_width` columns, markdown
/// rendered and long text wrapped to fit. Shared by row flattening (for counts)
/// and rendering (for content) so they can never disagree.
pub fn comment_display(
    comment: &Comment,
    row_width: u16,
    highlighter: Option<&Highlighter>,
) -> Vec<CommentLine> {
    let budget = card_budget(row_width);
    let mut lines = vec![CommentLine::Header];
    for logical in markdown::parse(&comment.body, highlighter) {
        lines.extend(
            markdown::wrap(&logical, budget, budget)
                .into_iter()
                .map(CommentLine::Body),
        );
    }
    for reply in &comment.replies {
        // the author label only renders on the first line; continuations get
        // the renderer's two-space indent
        let label = format!("└ {}: ", reply.author).width();
        let mut first = true;
        for logical in markdown::parse(&reply.body, highlighter) {
            let head = budget.saturating_sub(if first { label } else { 2 }).max(8);
            for spans in markdown::wrap(&logical, head, budget.saturating_sub(2).max(8)) {
                lines.push(CommentLine::Reply {
                    author: reply.author.clone(),
                    spans,
                    first,
                });
                first = false;
            }
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

/// The open composer and the rows it draws, carried through row building so
/// its rows land exactly where its result will.
struct Draft<'a> {
    composer: &'a Composer,
    height: usize,
}

impl Draft<'_> {
    fn new(composer: Option<&Composer>, wrap_width: u16) -> Option<Draft<'_>> {
        let composer = composer?;
        Some(Draft {
            height: composer.display(wrap_width).len(),
            composer,
        })
    }

    fn edits(&self, id: &str) -> bool {
        matches!(self.composer.kind, ComposerKind::Edit { .. })
            && self.composer.comment_id() == Some(id)
    }

    fn replies_to(&self, id: &str) -> bool {
        matches!(self.composer.kind, ComposerKind::Reply { .. })
            && self.composer.comment_id() == Some(id)
    }

    /// The hunk and line the composer's new comment will display under, or
    /// `None` when it is file-level or belongs to another file.
    fn new_at(&self, file: &FileDiff) -> Option<(usize, usize)> {
        let anchor = self.composer.anchor()?;
        (anchor.file == file.path).then(|| anchor_target(file, anchor))?
    }

    /// A file-level new comment: the composer opens above the diff, where a
    /// whole-file comment renders.
    fn is_unanchored(&self, file: &FileDiff) -> bool {
        self.composer
            .anchor()
            .is_some_and(|anchor| anchor.file == file.path && anchor_target(file, anchor).is_none())
    }
}

fn push_comment_rows(
    rows: &mut Vec<DiffRow>,
    session: &Session,
    comments: &[(usize, bool)],
    wrap_width: u16,
    draft: Option<&Draft<'_>>,
) {
    for &(comment, outdated) in comments {
        let Some(c) = session.comments.get(comment) else {
            continue;
        };
        if draft.is_some_and(|d| d.edits(&c.id)) {
            push_draft_rows(rows, draft);
            continue;
        }
        let count = comment_display(c, wrap_width, None).len();
        rows.extend((0..count).map(|line| DiffRow::Comment {
            comment,
            line,
            outdated,
        }));
        if draft.is_some_and(|d| d.replies_to(&c.id)) {
            push_draft_rows(rows, draft);
        }
    }
}

fn push_draft_rows(rows: &mut Vec<DiffRow>, draft: Option<&Draft<'_>>) {
    let Some(draft) = draft else { return };
    rows.extend((0..draft.height).map(|line| DiffRow::Composer { line }));
}

/// Bucket a file's comments by their `(hunk, line)` anchor for inline display.
/// A line anchor that no longer exists is outdated, and a file-level comment
/// has no line: both land in the unanchored list, rendered at the top.
type CommentBuckets = (
    HashMap<(usize, usize), Vec<(usize, bool)>>,
    Vec<(usize, bool)>,
);

fn collect_comments(file: &FileDiff, session: &Session, model: &DiffModel) -> CommentBuckets {
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
            None => unanchored.push((comment_idx, outdated)),
        }
    }
    (by_line, unanchored)
}

/// Build the diff-pane rows for one file: its hunks and lines, with comment
/// blocks under their anchored line, file-level (or orphaned) comments first.
pub(super) fn build_rows(
    model: &DiffModel,
    session: &Session,
    selected: usize,
    wrap_width: u16,
    composer: Option<&Composer>,
) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let Some(file) = model.files.get(selected) else {
        return rows;
    };
    let draft = Draft::new(composer, wrap_width);
    let draft = draft.as_ref();
    let (by_line, unanchored) = collect_comments(file, session, model);
    push_comment_rows(&mut rows, session, &unanchored, wrap_width, draft);
    if draft.is_some_and(|d| d.is_unanchored(file)) {
        push_draft_rows(&mut rows, draft);
    }
    let new_at = draft.and_then(|d| d.new_at(file));
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        rows.push(DiffRow::Hunk {
            file: selected,
            hunk: hunk_idx,
        });
        for line_idx in 0..hunk.lines.len() {
            rows.push(DiffRow::Line {
                file: selected,
                hunk: hunk_idx,
                line: line_idx,
            });
            if let Some(list) = by_line.get(&(hunk_idx, line_idx)) {
                push_comment_rows(&mut rows, session, list, wrap_width, draft);
            }
            if new_at == Some((hunk_idx, line_idx)) {
                push_draft_rows(&mut rows, draft);
            }
        }
    }
    rows
}

/// Which column of a side-by-side row a line belongs to: the old side renders
/// on the left, the new side on the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitSide {
    Left,
    Right,
}

/// One row of the side-by-side diff body. `left`/`right` index into the hunk's
/// lines: a context row carries the same index on both sides, a modified row
/// pairs a deletion with an addition, and a lone deletion or addition fills one
/// side with `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitRow {
    Hunk {
        hunk: usize,
    },
    Pair {
        hunk: usize,
        left: Option<usize>,
        right: Option<usize>,
    },
    Comment {
        comment: usize,
        line: usize,
        outdated: bool,
    },
    Composer {
        line: usize,
    },
}

fn push_split_comments(
    rows: &mut Vec<SplitRow>,
    session: &Session,
    comments: &[(usize, bool)],
    wrap_width: u16,
    draft: Option<&Draft<'_>>,
) {
    for &(comment, outdated) in comments {
        let Some(c) = session.comments.get(comment) else {
            continue;
        };
        if draft.is_some_and(|d| d.edits(&c.id)) {
            push_split_draft_rows(rows, draft);
            continue;
        }
        let count = comment_display(c, wrap_width, None).len();
        rows.extend((0..count).map(|line| SplitRow::Comment {
            comment,
            line,
            outdated,
        }));
        if draft.is_some_and(|d| d.replies_to(&c.id)) {
            push_split_draft_rows(rows, draft);
        }
    }
}

fn push_split_draft_rows(rows: &mut Vec<SplitRow>, draft: Option<&Draft<'_>>) {
    let Some(draft) = draft else { return };
    rows.extend((0..draft.height).map(|line| SplitRow::Composer { line }));
}

/// Emit a change block as aligned pairs: deletions on the left, additions on
/// the right, zipped by position with `None` filling the shorter side. Any
/// comment anchored to a paired line follows its row.
#[allow(clippy::too_many_arguments)]
fn flush_change_block(
    rows: &mut Vec<SplitRow>,
    session: &Session,
    by_line: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    hunk: usize,
    dels: &[usize],
    adds: &[usize],
    wrap_width: u16,
    draft: Option<&Draft<'_>>,
    new_at: Option<(usize, usize)>,
) {
    for k in 0..dels.len().max(adds.len()) {
        let left = dels.get(k).copied();
        let right = adds.get(k).copied();
        rows.push(SplitRow::Pair { hunk, left, right });
        for line in [left, right].into_iter().flatten() {
            if let Some(list) = by_line.get(&(hunk, line)) {
                push_split_comments(rows, session, list, wrap_width, draft);
            }
            if new_at == Some((hunk, line)) {
                push_split_draft_rows(rows, draft);
            }
        }
    }
}

/// Build the side-by-side rows for one file, the split-mode counterpart to
/// [`build_rows`]. Same comment placement; lines are paired old-to-new.
pub(super) fn build_split_rows(
    model: &DiffModel,
    session: &Session,
    selected: usize,
    wrap_width: u16,
    composer: Option<&Composer>,
) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let Some(file) = model.files.get(selected) else {
        return rows;
    };
    let draft = Draft::new(composer, wrap_width);
    let draft = draft.as_ref();
    let (by_line, unanchored) = collect_comments(file, session, model);
    push_split_comments(&mut rows, session, &unanchored, wrap_width, draft);
    if draft.is_some_and(|d| d.is_unanchored(file)) {
        push_split_draft_rows(&mut rows, draft);
    }
    let new_at = draft.and_then(|d| d.new_at(file));
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        rows.push(SplitRow::Hunk { hunk: hunk_idx });
        let mut dels: Vec<usize> = Vec::new();
        let mut adds: Vec<usize> = Vec::new();
        for (line_idx, line) in hunk.lines.iter().enumerate() {
            match line.kind {
                LineKind::Context => {
                    flush_change_block(
                        &mut rows, session, &by_line, hunk_idx, &dels, &adds, wrap_width, draft,
                        new_at,
                    );
                    dels.clear();
                    adds.clear();
                    rows.push(SplitRow::Pair {
                        hunk: hunk_idx,
                        left: Some(line_idx),
                        right: Some(line_idx),
                    });
                    if let Some(list) = by_line.get(&(hunk_idx, line_idx)) {
                        push_split_comments(&mut rows, session, list, wrap_width, draft);
                    }
                    if new_at == Some((hunk_idx, line_idx)) {
                        push_split_draft_rows(&mut rows, draft);
                    }
                }
                LineKind::Deleted => dels.push(line_idx),
                LineKind::Added => adds.push(line_idx),
            }
        }
        flush_change_block(
            &mut rows, session, &by_line, hunk_idx, &dels, &adds, wrap_width, draft, new_at,
        );
    }
    rows
}
