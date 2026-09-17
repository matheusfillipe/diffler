//! Flattening a file's diff and its review comments into the row list the
//! pane renders.

use std::collections::HashMap;

use diffler_core::highlight::Highlighter;
use diffler_core::model::{DiffModel, FileDiff, Hunk, LineKind};
use diffler_core::session::{Anchor, Comment, CommentStatus, Session};
use diffler_core::walkthrough::Located;
use unicode_width::UnicodeWidthStr;

use crate::app::composer::{Composer, ComposerKind, ComposerLine, card_budget};
use crate::app::markdown::{self, MdSpan};
use crate::app::walkthrough::{Block, FigureCache, unresolved_explanation};

/// One terminal row of the diff pane. Indices point into the model the view
/// renders; the row list is rebuilt whenever the selected file, the model, or
/// the session change, so they never dangle. The `file` field equals the
/// selected file index everywhere except the walkthrough layout's all-slides
/// view, which windows several files' rows one after another.
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
    /// One display line of the walkthrough's own summary card; `line` indexes
    /// the block produced by [`summary_display`].
    Summary {
        line: usize,
    },
}

/// One display line of a comment block. Body and reply text carry markdown
/// styling as flag-tagged runs; [`crate::ui`] maps the flags to concrete styles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentLine {
    Header,
    Body(Vec<MdSpan>),
    /// Row `row` of the figure that is block `block` of the comment's body.
    Figure {
        block: usize,
        row: usize,
    },
    /// One line explaining why this stop or note has no code to show,
    /// rendered dim under the body.
    Note(Vec<MdSpan>),
    Reply {
        author: String,
        spans: Vec<MdSpan>,
        first: bool,
    },
    Footer,
}

/// What a visual selection yanks for one row, built alongside `rows` (which
/// carries indices, not text) since [`crate::app::rowsel::RowText::row_text`]
/// answers with no model or session in hand. A figure is the one row this
/// cannot resolve here: its box-drawing depends on the theme-driven graph
/// renderer that lives in `ui`, so it starts as a lookup key and the render
/// pass that already draws the same figure patches the text in once it runs.
#[derive(Debug, Clone)]
pub enum RowCopy {
    Text(String),
    Figure {
        key: String,
        block: usize,
        row: usize,
    },
}

impl RowCopy {
    pub fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Figure { .. } => String::new(),
        }
    }
}

fn md_spans_text(spans: &[MdSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

/// The marker and text of one diff line, exactly as a plain-text buffer would
/// hold it: `+`/`-`/` ` then the line's own text, gutter numbers left out.
fn line_row_text(line: &diffler_core::model::DiffLine) -> String {
    let marker = match line.kind {
        LineKind::Added => '+',
        LineKind::Deleted => '-',
        LineKind::Context => ' ',
    };
    format!("{marker}{}", line.text)
}

/// A hunk row's header, the same ranges git itself prints.
fn hunk_header_text(hunk: &Hunk) -> String {
    let ranges = format!(
        "@@ -{},{} +{},{} @@",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
    );
    if hunk.context.is_empty() {
        ranges
    } else {
        format!("{ranges} {}", hunk.context)
    }
}

/// A comment card's header line as plain text: title (a stop's), author,
/// status, and the outdated/stale flags the card shows beside them.
fn comment_header_text(comment: &Comment, outdated: bool, unresolved: bool) -> String {
    let status = match comment.status {
        CommentStatus::Open => "open",
        CommentStatus::Replied => "replied",
        CommentStatus::Resolved => "resolved",
    };
    let mut text = String::new();
    if let Some(title) = comment.title.as_deref() {
        text.push_str(title);
        text.push_str("  ");
    }
    text.push_str(&comment.author);
    text.push_str(" · ");
    text.push_str(status);
    if outdated {
        text.push_str(" · outdated");
    }
    if unresolved {
        text.push_str(" · stale");
    }
    text
}

/// A composer row's text: the draft line itself for a body row, nothing for
/// the header/footer chrome around it.
fn composer_line_text(line: &ComposerLine) -> String {
    match line {
        ComposerLine::Body { text, .. } => text.clone(),
        ComposerLine::Header | ComposerLine::Footer => String::new(),
    }
}

/// What one display line of a comment or summary card yanks: its prose for a
/// body/note/reply line, so several selected rows read as the paragraph they
/// look like; `header` for the header line (built by the caller, since a
/// summary's is a bare title and a comment's carries status); nothing for the
/// footer, a bar with no content; a figure key for the caller's render pass
/// to resolve later.
pub(super) fn row_copy_for(line: &CommentLine, header: &str, key: &str) -> RowCopy {
    match line {
        CommentLine::Header => RowCopy::Text(header.to_owned()),
        CommentLine::Body(spans) | CommentLine::Note(spans) => RowCopy::Text(md_spans_text(spans)),
        CommentLine::Reply {
            author,
            spans,
            first,
        } => {
            let mut text = if *first {
                format!("└ {author}: ")
            } else {
                "  ".to_owned()
            };
            text.push_str(&md_spans_text(spans));
            RowCopy::Text(text)
        }
        CommentLine::Figure { block, row } => RowCopy::Figure {
            key: key.to_owned(),
            block: *block,
            row: *row,
        },
        CommentLine::Footer => RowCopy::Text(String::new()),
    }
}

/// The header, body and figures of `body` at `row_width` columns, markdown
/// rendered and long text wrapped to fit. `blocks` is the body already split
/// around its `mermaid` fences, for a body that holds one; without it the
/// whole body is prose. Shared by a comment's card and the walkthrough
/// summary's, so a `mermaid` fence renders as a figure through the same path
/// in both.
fn body_display(
    body: &str,
    row_width: u16,
    highlighter: Option<&Highlighter>,
    blocks: Option<&[Block]>,
) -> Vec<CommentLine> {
    let budget = card_budget(row_width);
    let mut lines = vec![CommentLine::Header];
    match blocks {
        Some(blocks) => {
            for (block, part) in blocks.iter().enumerate() {
                match part {
                    Block::Prose(prose) => {
                        lines.extend(prose.iter().cloned().map(CommentLine::Body));
                    }
                    Block::Figure(figure) => {
                        lines.extend(
                            (0..figure.rows()).map(|row| CommentLine::Figure { block, row }),
                        );
                    }
                }
            }
        }
        None => {
            for logical in markdown::parse(body, highlighter, budget) {
                lines.extend(
                    markdown::wrap(&logical, budget, budget)
                        .into_iter()
                        .map(CommentLine::Body),
                );
            }
        }
    }
    lines
}

/// The terminal lines a comment occupies at `row_width` columns. Shared by
/// row flattening (for counts) and rendering (for content) so they can never
/// disagree. `unresolved` is the reason this comment's own anchor stopped
/// resolving, when it did; it renders as one dim line under the body.
pub fn comment_display(
    comment: &Comment,
    row_width: u16,
    highlighter: Option<&Highlighter>,
    blocks: Option<&[Block]>,
    unresolved: Option<Located>,
) -> Vec<CommentLine> {
    let budget = card_budget(row_width);
    let mut lines = body_display(&comment.body, row_width, highlighter, blocks);
    if let Some(reason) =
        unresolved.and_then(|reason| unresolved_explanation(&comment.anchor.file, reason))
    {
        for logical in markdown::parse(&reason, None, budget) {
            lines.extend(
                markdown::wrap(&logical, budget, budget)
                    .into_iter()
                    .map(CommentLine::Note),
            );
        }
    }
    for reply in &comment.replies {
        // the author label only renders on the first line; continuations get
        // the renderer's two-space indent
        let label = format!("└ {}: ", reply.author).width();
        let mut first = true;
        // a table lays out at parse time and `wrap` then leaves it alone, so
        // it has to fit the narrowest line of the reply: the labelled one
        let laid_out = budget.saturating_sub(label.max(2)).max(8);
        for logical in markdown::parse(&reply.body, highlighter, laid_out) {
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

/// The terminal lines the walkthrough's own summary occupies: the same
/// header/body/figure/footer shape a comment's card gets, with no replies
/// since nothing answers a summary directly.
pub fn summary_display(
    body: &str,
    row_width: u16,
    highlighter: Option<&Highlighter>,
    blocks: Option<&[Block]>,
) -> Vec<CommentLine> {
    let mut lines = body_display(body, row_width, highlighter, blocks);
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
/// its rows land exactly where its result will. `lines` is computed once and
/// doubles as the height, so text and count can never drift apart.
struct Draft<'a> {
    composer: &'a Composer,
    lines: Vec<ComposerLine>,
}

impl Draft<'_> {
    fn new(composer: Option<&Composer>, wrap_width: u16) -> Option<Draft<'_>> {
        let composer = composer?;
        Some(Draft {
            lines: composer.display(wrap_width),
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

/// The inputs a card row's build needs but that never vary within one row
/// pass: the session its comments live in, the width bodies wrap to, the
/// composer's open draft when there is one, the figure cache, and which
/// comments' anchors stopped resolving. Bundled the way `TreeRowCtx` bundles
/// a sidebar row's render inputs (`crate::ui::diff`), so they travel as one
/// value through row building instead of as five separate parameters.
#[derive(Clone, Copy)]
struct RowCtx<'a> {
    session: &'a Session,
    wrap_width: u16,
    draft: Option<&'a Draft<'a>>,
    figures: &'a FigureCache,
    unresolved_anchors: &'a HashMap<String, Located>,
}

fn push_comment_rows(
    rows: &mut Vec<DiffRow>,
    copy: &mut Vec<RowCopy>,
    comments: &[(usize, bool)],
    ctx: RowCtx<'_>,
) {
    for &(comment, outdated) in comments {
        let Some(c) = ctx.session.comments.get(comment) else {
            continue;
        };
        if ctx.draft.is_some_and(|d| d.edits(&c.id)) {
            push_draft_rows(rows, copy, ctx.draft);
            continue;
        }
        let unresolved = ctx.unresolved_anchors.get(&c.id).copied();
        let lines = comment_display(
            c,
            ctx.wrap_width,
            None,
            blocks_of(ctx.figures, &c.id),
            unresolved,
        );
        let header = comment_header_text(c, outdated, unresolved.is_some());
        for (line, part) in lines.iter().enumerate() {
            rows.push(DiffRow::Comment {
                comment,
                line,
                outdated,
            });
            copy.push(row_copy_for(part, &header, &c.id));
        }
        if ctx.draft.is_some_and(|d| d.replies_to(&c.id)) {
            push_draft_rows(rows, copy, ctx.draft);
        }
    }
}

fn push_draft_rows(rows: &mut Vec<DiffRow>, copy: &mut Vec<RowCopy>, draft: Option<&Draft<'_>>) {
    let Some(draft) = draft else { return };
    for (line, part) in draft.lines.iter().enumerate() {
        rows.push(DiffRow::Composer { line });
        copy.push(RowCopy::Text(composer_line_text(part)));
    }
}

/// A comment's body already split around its figures, for the ones that have
/// any. Row building and rendering both read it from here, so they count the
/// same lines.
pub fn blocks_of<'a>(figures: &'a FigureCache, id: &str) -> Option<&'a [Block]> {
    Some(figures.get(id)?.blocks.as_slice())
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
/// `copy` is what a visual selection yanks for each row, built in the same
/// pass so it can never disagree with what `rows` holds.
pub(super) fn build_rows(
    model: &DiffModel,
    session: &Session,
    selected: usize,
    wrap_width: u16,
    composer: Option<&Composer>,
    figures: &FigureCache,
    unresolved_anchors: &HashMap<String, Located>,
) -> (Vec<DiffRow>, Vec<RowCopy>) {
    let mut rows = Vec::new();
    let mut copy = Vec::new();
    let Some(file) = model.files.get(selected) else {
        return (rows, copy);
    };
    let draft = Draft::new(composer, wrap_width);
    let ctx = RowCtx {
        session,
        wrap_width,
        draft: draft.as_ref(),
        figures,
        unresolved_anchors,
    };
    let (by_line, unanchored) = collect_comments(file, session, model);
    push_comment_rows(&mut rows, &mut copy, &unanchored, ctx);
    if ctx.draft.is_some_and(|d| d.is_unanchored(file)) {
        push_draft_rows(&mut rows, &mut copy, ctx.draft);
    }
    let new_at = ctx.draft.and_then(|d| d.new_at(file));
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        rows.push(DiffRow::Hunk {
            file: selected,
            hunk: hunk_idx,
        });
        copy.push(RowCopy::Text(hunk_header_text(hunk)));
        for (line_idx, line) in hunk.lines.iter().enumerate() {
            rows.push(DiffRow::Line {
                file: selected,
                hunk: hunk_idx,
                line: line_idx,
            });
            copy.push(RowCopy::Text(line_row_text(line)));
            if let Some(list) = by_line.get(&(hunk_idx, line_idx)) {
                push_comment_rows(&mut rows, &mut copy, list, ctx);
            }
            if new_at == Some((hunk_idx, line_idx)) {
                push_draft_rows(&mut rows, &mut copy, ctx.draft);
            }
        }
    }
    (rows, copy)
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

fn push_split_comments(rows: &mut Vec<SplitRow>, comments: &[(usize, bool)], ctx: RowCtx<'_>) {
    for &(comment, outdated) in comments {
        let Some(c) = ctx.session.comments.get(comment) else {
            continue;
        };
        if ctx.draft.is_some_and(|d| d.edits(&c.id)) {
            push_split_draft_rows(rows, ctx.draft);
            continue;
        }
        let unresolved = ctx.unresolved_anchors.get(&c.id).copied();
        let count = comment_display(
            c,
            ctx.wrap_width,
            None,
            blocks_of(ctx.figures, &c.id),
            unresolved,
        )
        .len();
        rows.extend((0..count).map(|line| SplitRow::Comment {
            comment,
            line,
            outdated,
        }));
        if ctx.draft.is_some_and(|d| d.replies_to(&c.id)) {
            push_split_draft_rows(rows, ctx.draft);
        }
    }
}

fn push_split_draft_rows(rows: &mut Vec<SplitRow>, draft: Option<&Draft<'_>>) {
    let Some(draft) = draft else { return };
    rows.extend((0..draft.lines.len()).map(|line| SplitRow::Composer { line }));
}

/// Emit a change block as aligned pairs: deletions on the left, additions on
/// the right, zipped by position with `None` filling the shorter side. Any
/// comment anchored to a paired line follows its row.
fn flush_change_block(
    rows: &mut Vec<SplitRow>,
    by_line: &HashMap<(usize, usize), Vec<(usize, bool)>>,
    hunk: usize,
    dels: &[usize],
    adds: &[usize],
    new_at: Option<(usize, usize)>,
    ctx: RowCtx<'_>,
) {
    for k in 0..dels.len().max(adds.len()) {
        let left = dels.get(k).copied();
        let right = adds.get(k).copied();
        rows.push(SplitRow::Pair { hunk, left, right });
        for line in [left, right].into_iter().flatten() {
            if let Some(list) = by_line.get(&(hunk, line)) {
                push_split_comments(rows, list, ctx);
            }
            if new_at == Some((hunk, line)) {
                push_split_draft_rows(rows, ctx.draft);
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
    figures: &FigureCache,
    unresolved_anchors: &HashMap<String, Located>,
) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let Some(file) = model.files.get(selected) else {
        return rows;
    };
    let draft = Draft::new(composer, wrap_width);
    let ctx = RowCtx {
        session,
        wrap_width,
        draft: draft.as_ref(),
        figures,
        unresolved_anchors,
    };
    let (by_line, unanchored) = collect_comments(file, session, model);
    push_split_comments(&mut rows, &unanchored, ctx);
    if ctx.draft.is_some_and(|d| d.is_unanchored(file)) {
        push_split_draft_rows(&mut rows, ctx.draft);
    }
    let new_at = ctx.draft.and_then(|d| d.new_at(file));
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        rows.push(SplitRow::Hunk { hunk: hunk_idx });
        let mut dels: Vec<usize> = Vec::new();
        let mut adds: Vec<usize> = Vec::new();
        for (line_idx, line) in hunk.lines.iter().enumerate() {
            match line.kind {
                LineKind::Context => {
                    flush_change_block(&mut rows, &by_line, hunk_idx, &dels, &adds, new_at, ctx);
                    dels.clear();
                    adds.clear();
                    rows.push(SplitRow::Pair {
                        hunk: hunk_idx,
                        left: Some(line_idx),
                        right: Some(line_idx),
                    });
                    if let Some(list) = by_line.get(&(hunk_idx, line_idx)) {
                        push_split_comments(&mut rows, list, ctx);
                    }
                    if new_at == Some((hunk_idx, line_idx)) {
                        push_split_draft_rows(&mut rows, ctx.draft);
                    }
                }
                LineKind::Deleted => dels.push(line_idx),
                LineKind::Added => adds.push(line_idx),
            }
        }
        flush_change_block(&mut rows, &by_line, hunk_idx, &dels, &adds, new_at, ctx);
    }
    rows
}
