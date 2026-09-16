//! Reading the walkthrough an agent wrote about the open review: parsing each
//! stop's markdown into blocks, and resolving the code its anchors name.
//!
//! A body parses into blocks the moment the view is built, which is pure work
//! over a string. Anchors are a different matter: resolving one reads its file
//! and parses it, so it runs on the blocking pool like enrichment and the
//! stops render unseated until the answer lands.

use std::collections::{HashMap, HashSet};

use diffler_core::model::{DiffLine, FileDiff, FileStatus, HashCache, Hunk, LineKind, hunk_id};
use diffler_core::session::{Comment, Session};
use diffler_core::source::ReviewSource;
use diffler_core::walkthrough::{Located, Target, Walkthrough};

use crate::app::diff::Slide;
use crate::app::markdown::MdSpan;
use crate::app::{
    App, CommentLine, DiffRow, Flow, Modal, PendingOp, Screen, blocks_of, comment_display,
    summary_display,
};
use crate::graph::{Fit, GraphView, NodeId};

/// One element of a stop's body, in reading order.
#[derive(Debug)]
pub enum Block {
    Prose(Vec<Vec<MdSpan>>),
    Figure(Box<FigureBlock>),
}

#[derive(Debug)]
pub struct FigureBlock {
    pub view: GraphView,
    /// What each node's `click` named, before any file was read.
    pub targets: Vec<(NodeId, Target)>,
    /// Where each node's target landed, once resolved. A node absent from the
    /// map has no anchor at all.
    pub resolved: HashMap<NodeId, Located>,
    pub resolved_yet: bool,
    /// Whether the figure drew as its author declared it, was redrawn
    /// top-down to fit the card, or still overflows even so.
    pub fit: Fit,
}

impl FigureBlock {
    /// Nodes whose anchor stopped resolving, counted for the figure header so
    /// a walkthrough that has aged says so.
    pub fn stale(&self) -> usize {
        self.resolved
            .values()
            .filter(|located| matches!(located, Located::Lost | Located::FileMissing))
            .count()
    }

    /// Terminal rows the figure occupies: its header, the graph itself, and
    /// one more when it was redrawn or cropped to fit and has to say so. Row
    /// building and rendering both read it, so they cannot disagree.
    pub fn rows(&self) -> usize {
        1 + self.view.height().clamp(1, FIGURE_MAX_ROWS) as usize
            + usize::from(self.fit != Fit::AsDrawn)
    }
}

/// Rows a figure may take before it is cropped. A chain drawn downward spends
/// five rows a node, so this holds six of them; the pane scrolls past it.
pub const FIGURE_MAX_ROWS: u16 = 32;

/// One comment body already split into blocks, so a `mermaid` fence is parsed
/// once rather than on every frame that draws a row of it. `hash` covers the
/// body and the width it was wrapped to, which are the two things that make
/// it stale.
#[derive(Debug)]
pub struct CachedBody {
    pub hash: u64,
    pub blocks: Vec<Block>,
}

/// Every comment body that holds a figure, by comment id. Bodies without one
/// are absent: they render straight from markdown and cost nothing to keep.
pub type FigureCache = std::collections::HashMap<String, CachedBody>;

/// Whether a body holds a diagram at all, which is what earns it a cache entry.
pub fn has_figure(body: &str) -> bool {
    body.lines().any(is_mermaid_fence)
}

/// Key `ensure_figures`/`rasterize_figures` cache the walkthrough's own
/// summary figures under, distinct from any comment id (always a bare UUID).
pub fn summary_figure_key(walkthrough_id: &str) -> String {
    format!("summary:{walkthrough_id}")
}

pub fn body_hash(body: &str, width: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    width.hash(&mut hasher);
    hasher.finish()
}

/// A queued anchor resolution. The token drops an answer for a walkthrough the
/// agent has already replaced.
#[derive(Debug, Clone)]
pub struct WalkthroughRequest {
    pub token: u64,
    pub files: Vec<String>,
    /// The revision to read `files` from, when the walkthrough was published
    /// with one; `None` reads the live worktree, for a walkthrough published
    /// before `rev` existed.
    pub read_rev: Option<String>,
}

/// One run of a stop body: prose, or the source of a mermaid fence.
enum Chunk {
    Prose(String),
    Mermaid(String),
}

/// Split a body on ` ```mermaid ` fences. Every other fence stays prose, so a
/// code sample in a stop still renders as code.
fn chunks(body: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut prose = String::new();
    let mut fence: Option<String> = None;
    for line in body.lines() {
        match fence.as_mut() {
            Some(collected) => {
                if line.trim_start().starts_with("```") {
                    chunks.push(Chunk::Prose(std::mem::take(&mut prose)));
                    chunks.push(Chunk::Mermaid(std::mem::take(collected)));
                    fence = None;
                } else {
                    collected.push_str(line);
                    collected.push('\n');
                }
            }
            None if is_mermaid_fence(line) => fence = Some(String::new()),
            None => {
                prose.push_str(line);
                prose.push('\n');
            }
        }
    }
    // an unterminated fence is prose: its source is more use than a gap
    if let Some(unterminated) = fence {
        prose.push_str(&unterminated);
    }
    chunks.push(Chunk::Prose(prose));
    chunks
}

pub fn blocks(body: &str, width: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut prose = String::new();
    for chunk in chunks(body) {
        match chunk {
            Chunk::Prose(text) => prose.push_str(&text),
            Chunk::Mermaid(src) => {
                if let Some(figure) = figure_block(&src, width) {
                    push_prose(&mut blocks, &mut prose, width);
                    blocks.push(figure);
                } else {
                    // a diagram we cannot draw still has to reach the reader
                    prose.push_str("```\n");
                    prose.push_str(&src);
                    prose.push_str("```\n");
                }
            }
        }
    }
    push_prose(&mut blocks, &mut prose, width);
    blocks
}

/// Figures a body would draw, and what drawing them simplified. What the MCP
/// write path answers with, so an agent learns the subset without the reader
/// ever seeing a broken figure.
pub fn validate(body: &str) -> (usize, Vec<String>) {
    let mut figures = 0;
    let mut notes = Vec::new();
    for (index, src) in chunks(body)
        .iter()
        .filter_map(|chunk| match chunk {
            Chunk::Mermaid(src) => Some(src),
            Chunk::Prose(_) => None,
        })
        .enumerate()
    {
        let at = index + 1;
        match crate::graph::mermaid::parse(src) {
            Ok(figure) => {
                figures += 1;
                notes.extend(
                    figure
                        .notes
                        .iter()
                        .map(|note| format!("diagram {at}: {note}")),
                );
            }
            Err(err) => notes.push(format!("diagram {at}: {err}; left as source")),
        }
    }
    (figures, notes)
}

fn is_mermaid_fence(line: &str) -> bool {
    let rest = line.trim_start().strip_prefix("```").unwrap_or_default();
    rest.trim().eq_ignore_ascii_case("mermaid")
}

fn push_prose(blocks: &mut Vec<Block>, prose: &mut String, width: usize) {
    if prose.trim().is_empty() {
        prose.clear();
        return;
    }
    // `parse` lays tables out to the width and leaves prose in logical lines;
    // wrapping them is the caller's job, and a table row passes through
    let wrapped = crate::app::markdown::parse(prose, None, width)
        .iter()
        .flat_map(|runs| crate::app::markdown::wrap(runs, width, width))
        .collect();
    blocks.push(Block::Prose(wrapped));
    prose.clear();
}

/// Lay a `mermaid` fence's figure out to fit `width` columns: the direction
/// its author drew if that fits, top-down if it does not (a chain always
/// fits a card's width running downward), cropped as a last resort.
fn figure_block(src: &str, width: usize) -> Option<Block> {
    let figure = crate::graph::mermaid::parse(src).ok()?;
    let targets: Vec<(NodeId, Target)> = figure
        .anchors
        .iter()
        .map(|(id, raw)| (id.clone(), Target::parse(raw)))
        .collect();
    let mut view = GraphView::new();
    let fit = view.set_model_fit(figure.model, u16::try_from(width).unwrap_or(u16::MAX));
    // a card figure is a static picture, not something being navigated, so
    // it never asked for the default selection `set_model` just gave it
    view.clear_selection();
    let resolved_yet = targets.is_empty();
    Some(Block::Figure(Box::new(FigureBlock {
        view,
        targets,
        resolved: HashMap::new(),
        resolved_yet,
        fit,
    })))
}

pub fn figure_count(body: &str) -> usize {
    validate(body).0
}

/// A figure's `click` targets that resolved to a real span, as `(path, line,
/// end)`: what the Graph screen needs to jump `<cr>` on a node to its code.
fn figure_anchor_targets(figure: &FigureBlock) -> HashMap<NodeId, (String, u32, u32)> {
    figure
        .targets
        .iter()
        .filter_map(|(id, target)| match figure.resolved.get(id) {
            Some(Located::Found { line, end }) => {
                Some((id.clone(), (target.path().to_owned(), *line, *end)))
            }
            _ => None,
        })
        .collect()
}

/// The comments one slide holds, in reading order: its primary and everything
/// anchored inside the region that primary covers. A primary with no line
/// holds the other file-level cards of its file, since a slide is what the
/// pane shows and those sit in the same place.
pub fn slide_comments(session: &Session, primary: usize) -> Vec<usize> {
    let Some(anchor) = session.comments.get(primary).map(|c| &c.anchor) else {
        return Vec::new();
    };
    let span = anchor.span();
    let mut held: Vec<usize> = session
        .comments
        .iter()
        .enumerate()
        .filter(|(index, comment)| {
            if *index == primary {
                return true;
            }
            if comment.anchor.file != anchor.file
                || comment.anchor.on_old_side != anchor.on_old_side
            {
                return false;
            }
            // a card renders under the last line it covers, so that is the
            // line that has to fall inside the region
            match (span, comment.anchor.line_end.or(comment.anchor.line)) {
                (Some((start, end)), Some(at)) => start <= at && at <= end,
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => false,
            }
        })
        .map(|(index, _)| index)
        .collect();
    held.sort_by_key(|index| {
        let line = session
            .comments
            .get(*index)
            .and_then(|comment| comment.anchor.line)
            .unwrap_or(0);
        (line, *index != primary)
    });
    held
}

/// One `FileDiff` for a stop or note anchored at a file the diff itself does
/// not carry: a single hunk of context lines, so the walkthrough layout can
/// window to it exactly as it windows to a file the diff does carry.
fn context_file_diff(path: &str, content: &str) -> FileDiff {
    let lines: Vec<DiffLine> = content
        .lines()
        .enumerate()
        .map(|(index, text)| {
            let no = u32::try_from(index + 1).unwrap_or(u32::MAX);
            DiffLine::new(LineKind::Context, Some(no), Some(no), text.to_owned())
        })
        .collect();
    let line_count = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let hunk = Hunk {
        id: hunk_id(path, &lines),
        old_start: 1,
        old_lines: line_count,
        new_start: 1,
        new_lines: line_count,
        context: String::new(),
        lines,
    };
    FileDiff {
        path: path.to_owned(),
        old_path: None,
        status: FileStatus::Unchanged,
        binary: false,
        old_text: Some(content.to_owned()),
        new_text: Some(content.to_owned()),
        hunks: vec![hunk],
        hashes: HashCache::default(),
    }
}

/// What a stop is called wherever one is listed: the agent's own title, or
/// the opening line of its body when it wrote none.
pub fn stop_title(comment: &Comment) -> String {
    comment.title.clone().unwrap_or_else(|| {
        comment
            .body
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .to_owned()
    })
}

/// One line under a stop or note's body explaining why it has no code to
/// show: the file itself is missing, or the file is there but the symbol or
/// line the anchor names is gone from it. `None` for `Found`/`Whole`, which
/// are not failures.
pub fn unresolved_explanation(file: &str, reason: Located) -> Option<String> {
    match reason {
        Located::FileMissing => Some(format!(
            "`{file}` isn't in this checkout; open the revision the walkthrough was written against to see it"
        )),
        Located::Lost => Some(format!(
            "the anchor in `{file}` is gone; open the file to see what replaced it"
        )),
        Located::Found { .. } | Located::Whole => None,
    }
}

/// How much of the walkthrough has been read, for any header that counts its
/// stops: `seen/total` once at least one is marked, else just the total, the
/// way every other group header counts what it holds.
pub fn progress_label(session: &Session, walkthrough: &Walkthrough) -> String {
    let total = walkthrough.stops.len();
    let seen = walkthrough
        .stops
        .iter()
        .filter(|id| session.is_stop_seen(id))
        .count();
    if seen > 0 {
        format!("{seen}/{total}")
    } else {
        total.to_string()
    }
}

impl App {
    /// The active source's walkthrough: the one the open diff is showing, or
    /// `None` when the open source (or no diff at all) has none.
    pub(crate) fn active_walkthrough(&self) -> Option<&Walkthrough> {
        let diff = self.diff.as_ref()?;
        self.review.session_for(&diff.source).walkthrough.as_ref()
    }

    /// The comment stop `index` of the open review's walkthrough is.
    #[cfg(test)]
    pub(crate) fn stop_comment(&self, index: usize) -> Option<&Comment> {
        let id = self.active_walkthrough()?.stops.get(index)?;
        self.review
            .session_for(&self.active_review_source())
            .comment(id)
    }

    /// `<cr>` on a status row: open the walkthrough `id` names, in its own
    /// order, seated on `slide`.
    pub(crate) fn open_walkthrough(&mut self, id: &str, slide: Slide) {
        self.open_walkthrough_diff(id);
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        diff.layout = crate::config::FileLayout::Walkthrough;
        diff.mark_rows_dirty();
        match slide {
            Slide::Stop(index) => self.seat_stop(index),
            Slide::Summary => self.seat_summary(),
            Slide::AdHoc(id) => {
                self.focus_comment(&id);
            }
        }
    }

    /// The figure-cache key and block index of the figure the cursor's row
    /// belongs to, whether it sits under a comment's card or the summary's.
    fn figure_at_cursor(&self) -> Option<(String, usize)> {
        let diff = self.diff.as_ref()?;
        let row = diff.rows().get(diff.cursor)?;
        let session = self.review.session_for(&diff.source);
        match *row {
            DiffRow::Comment { comment, line, .. } => {
                let comment = session.comments.get(comment)?;
                let blocks = blocks_of(&diff.figures, &comment.id);
                let unresolved = diff.unresolved_anchors.get(&comment.id).copied();
                let lines = comment_display(comment, diff.wrap_width, None, blocks, unresolved);
                match lines.get(line)? {
                    CommentLine::Figure { block, .. } => Some((comment.id.clone(), *block)),
                    _ => None,
                }
            }
            DiffRow::Summary { line } => {
                let walkthrough = diff.active_walkthrough(session)?;
                let body = walkthrough.summary.as_ref()?;
                let key = summary_figure_key(&walkthrough.id);
                let blocks = blocks_of(&diff.figures, &key);
                let lines = summary_display(body, diff.wrap_width, None, blocks);
                match lines.get(line)? {
                    CommentLine::Figure { block, .. } => Some((key, *block)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `o` in the diff pane: open the figure under the cursor full-screen on
    /// the Graph screen, its default selection restored (a card figure
    /// clears its own) and every resolved `click` anchor ready for `<cr>`.
    /// `q`/back pops back to this same slide and cursor, since nothing here
    /// touches the diff view's own state.
    pub(crate) fn open_figure_graph_at_cursor(&mut self) {
        let Some((key, block)) = self.figure_at_cursor() else {
            self.info("no figure under the cursor");
            return;
        };
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let Some(Block::Figure(figure)) = diff.figures.get(&key).and_then(|c| c.blocks.get(block))
        else {
            return;
        };
        let mut view = GraphView::new();
        view.set_model(figure.view.model().clone());
        self.figure_graph_anchors = Some(figure_anchor_targets(figure));
        self.graph = Some(view);
        self.push_screen(Screen::Graph);
    }

    /// Notice the open source's walkthrough being revised under a reader, so
    /// an agent's republish shows without a restart. The row build is what
    /// parses the bodies, so run it here too and let a figure that only
    /// appeared there have its files read as well.
    pub(crate) fn ensure_walkthrough_view(&mut self) {
        let review = &self.review;
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        let source = diff.source.clone();
        let session = review.session_for(&source);
        let current = diff
            .active_walkthrough(session)
            .map(|walkthrough| (walkthrough.id.clone(), walkthrough.at));
        let mut changed = diff.walkthrough_built != current;
        if changed {
            diff.walkthrough_built = current;
            diff.mark_rows_dirty();
        }
        diff.ensure_rows(review);
        changed |= diff.take_figures_dirty();
        if changed {
            self.queue_walkthrough_anchors();
        }
    }

    /// Every distinct file the open source's own comments anchor to, and the
    /// `click` targets of every figure alike: one read serves both. Every
    /// comment in a walkthrough source's session is that walkthrough's, so no
    /// further filtering is needed; a human comment (no `anchor_ref`) drops
    /// out on its own.
    fn anchored_files(&self) -> Vec<String> {
        let session = self.review.session_for(&self.active_review_source());
        let mut files: Vec<String> = session
            .comments
            .iter()
            .filter_map(|c| c.anchor_ref.as_deref())
            .map(|anchor| Target::parse(anchor).path().to_owned())
            .collect();
        files.extend(
            self.diff
                .iter()
                .flat_map(|diff| diff.figures.values())
                .flat_map(|cached| &cached.blocks)
                .filter_map(|block| match block {
                    Block::Figure(figure) => Some(figure),
                    Block::Prose(_) => None,
                })
                .flat_map(|figure| figure.targets.iter().map(|(_, t)| t.path().to_owned())),
        );
        files.sort_unstable();
        files.dedup();
        files
    }

    pub(crate) fn queue_walkthrough_anchors(&mut self) {
        let files = self.anchored_files();
        if files.is_empty() {
            return;
        }
        let read_rev = self.active_walkthrough().and_then(|w| w.rev.clone());
        self.walkthrough_token = self.walkthrough_token.wrapping_add(1);
        self.pending_walkthrough = Some(WalkthroughRequest {
            token: self.walkthrough_token,
            files,
            read_rev,
        });
    }

    /// Fold the worker's file reads into every stop comment's anchor and every
    /// figure's node table. A stop then carries the anchor fields every other
    /// comment has, so outdated detection and card placement are the ones the
    /// review already runs.
    pub(crate) fn on_walkthrough_anchors(
        &mut self,
        contents: &HashMap<String, String>,
        pin_broken: bool,
        token: u64,
    ) -> Flow {
        if token != self.walkthrough_token {
            return Flow::Idle;
        }
        // only the file read can tell an absent file from a symbol gone from
        // a file that is still there: `Target::locate` only ever runs once
        // content is in hand, so it never has to guess which one happened
        let locate = |target: &Target| {
            contents
                .get(target.path())
                .map_or(Located::FileMissing, |content| target.locate(content))
        };

        let source = self.active_review_source();
        let model = self.source_model(&source);
        let stops = self
            .active_walkthrough()
            .map_or_else(Vec::new, |walkthrough| walkthrough.stops.clone());
        let session = self.review.session_for_mut(&source);
        let owned: Vec<String> = session.comments.iter().map(|c| c.id.clone()).collect();
        let mut unresolved = HashMap::new();
        // every path an owned comment anchors to, whether or not the diff
        // itself carries it: `context_files` below fills in the ones it does
        // not, so a stop can still window to them
        let mut context_paths = HashSet::new();
        for id in owned {
            let Some(comment) = session.comments.iter_mut().find(|c| c.id == id) else {
                continue;
            };
            // an anchor-less stop still needs its own (fallback) file found
            // in the model, or its card has nowhere to seat either
            context_paths.insert(comment.anchor.file.clone());
            let Some(anchor_ref) = comment.anchor_ref.clone() else {
                continue;
            };
            let target = Target::parse(&anchor_ref);
            context_paths.insert(target.path().to_owned());
            // a note marks a point inside its stop's region rather than a
            // second span, and one with no anchor of its own rides the stop's
            let point = !stops.contains(&id);
            comment.anchor.on_old_side = false;
            match locate(&target) {
                Located::Found { line, end } => {
                    let end = if point { line } else { end };
                    let text = model
                        .find_line(&comment.anchor.file, end, false)
                        .map(|found| found.text.clone());
                    comment.anchor.line = Some(line);
                    comment.anchor.line_end = (end > line).then_some(end);
                    comment.anchor.line_text = text;
                }
                Located::Whole => {
                    comment.anchor.line = None;
                    comment.anchor.line_end = None;
                    comment.anchor.line_text = None;
                }
                reason @ (Located::Lost | Located::FileMissing) => {
                    comment.anchor.line = None;
                    comment.anchor.line_end = None;
                    comment.anchor.line_text = None;
                    unresolved.insert(id, reason);
                }
            }
        }
        let _ = self.persist_review_change(&source);

        let Some(diff) = self.diff.as_mut() else {
            return Flow::Continue;
        };
        diff.pin_broken = pin_broken;
        // a path with no content read (the file exists nowhere reachable)
        // still gets an (empty) context file, so its comment's own file is
        // always found in the model: a slide never comes up with no card to
        // show for it, only nothing to show under it
        diff.context_files = context_paths
            .into_iter()
            .map(|path| {
                let content = contents.get(&path).map(String::as_str).unwrap_or_default();
                context_file_diff(&path, content)
            })
            .collect();
        for cached in diff.figures.values_mut() {
            for block in &mut cached.blocks {
                let Block::Figure(figure) = block else {
                    continue;
                };
                figure.resolved = figure
                    .targets
                    .iter()
                    .map(|(id, target)| (id.clone(), locate(target)))
                    .collect();
                figure.resolved_yet = true;
            }
        }
        diff.unresolved_anchors = unresolved;
        // where a card sits depends on where its anchor landed
        diff.mark_rows_dirty();
        // the reader is already standing on a slide that had nothing to seat
        // on, so its span has to appear under them rather than on the next
        // visit
        let slide =
            (diff.layout == crate::config::FileLayout::Walkthrough).then(|| diff.slide.clone());
        match slide {
            Some(Some(Slide::AdHoc(id))) => {
                self.focus_comment(&id);
            }
            Some(Some(Slide::Stop(index))) => self.seat_stop(index),
            Some(Some(Slide::Summary)) => self.seat_summary(),
            Some(None) => self.seat_stop(0),
            None => {}
        }
        Flow::Continue
    }

    /// Ask before deleting the walkthrough `id`'s review file and every
    /// comment it holds.
    pub(crate) fn confirm_delete_walkthrough(&mut self, id: &str) {
        let source = ReviewSource::Walkthrough { id: id.to_owned() };
        if let Err(err) = self.review.ensure_source(&source) {
            self.error(err.to_string());
            return;
        }
        let Some(walkthrough) = self.review.session_for(&source).walkthrough.as_ref() else {
            return;
        };
        self.modal = Some(Modal::Confirm {
            message: format!(
                "Delete the walkthrough \"{}\" and its comments?",
                walkthrough.title
            ),
            on_confirm: PendingOp::DeleteWalkthrough(id.to_owned()),
        });
    }

    /// Ask before deleting one stop and its notes, keeping the rest of the
    /// walkthrough.
    pub(crate) fn confirm_delete_stop(&mut self, index: usize) {
        self.modal = Some(Modal::Confirm {
            message: "Delete this stop and its notes?".to_owned(),
            on_confirm: PendingOp::DeleteStop(index),
        });
    }

    /// Delete the walkthrough `id`'s review file entirely, closing the diff
    /// first when it is the one open.
    pub(crate) fn delete_walkthrough(&mut self, id: &str) {
        let source = ReviewSource::Walkthrough { id: id.to_owned() };
        let title = self
            .review
            .session_for(&source)
            .walkthrough
            .as_ref()
            .map(|w| w.title.clone());
        if self.diff.as_ref().is_some_and(|d| d.source == source) && self.screen() == Screen::Diff {
            self.pop_screen();
        }
        if let Err(err) = diffler_core::store::delete_source(&self.review.repo_root, &source) {
            self.error(err.to_string());
            return;
        }
        self.review.forget_source(&source);
        self.reload_walkthroughs();
        self.info(match title {
            Some(title) => format!("deleted the walkthrough \"{title}\""),
            None => "deleted the walkthrough".to_owned(),
        });
    }

    /// Remove one stop and its notes from the active walkthrough.
    pub(crate) fn delete_stop(&mut self, index: usize) {
        let source = self.active_review_source();
        let session = self.review.session_for_mut(&source);
        if !session.delete_stop(index) {
            return;
        }
        let _ = self.persist_review_change(&source);
        self.reload_walkthroughs();
        self.info("deleted the stop");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "\
Four passes, and only the fields a layer set are folded on.

```mermaid
flowchart LR
  load[load defaults] --> merge[merge]
  click merge \"src/lib.rs#merge\"
```

The fourth is the one this diff changes.
";

    #[test]
    fn a_body_splits_into_prose_and_figures() {
        let blocks = blocks(BODY, 80);
        assert!(
            matches!(blocks.first(), Some(Block::Prose(_))),
            "{blocks:?}"
        );
        assert!(
            matches!(blocks.get(1), Some(Block::Figure(_))),
            "{blocks:?}"
        );
        assert!(matches!(blocks.get(2), Some(Block::Prose(_))), "{blocks:?}");
        assert_eq!(blocks.len(), 3);
    }

    #[test]
    fn a_figures_click_becomes_a_target() {
        let blocks = blocks(BODY, 80);
        let Some(Block::Figure(figure)) = blocks.get(1) else {
            panic!("a figure: {blocks:?}");
        };
        assert_eq!(
            figure.targets,
            [(
                NodeId::new("merge"),
                Target::Symbol {
                    path: "src/lib.rs".to_owned(),
                    symbol: "merge".to_owned()
                }
            )]
        );
        assert_eq!(figure.stale(), 0, "nothing is stale before resolving");
    }

    /// A code sample is not a diagram: only a mermaid fence becomes a figure.
    #[test]
    fn an_ordinary_code_fence_stays_prose() {
        let blocks = blocks("text\n\n```rust\nfn main() {}\n```\n", 80);
        assert!(
            blocks.iter().all(|b| matches!(b, Block::Prose(_))),
            "{blocks:?}"
        );
    }

    /// A diagram we cannot draw still has to reach the reader, as its source.
    #[test]
    fn an_unusable_diagram_falls_back_to_its_source() {
        let blocks = blocks("```mermaid\nsequenceDiagram\n  a->>b: hi\n```\n", 80);
        assert_eq!(blocks.len(), 1);
        let text = text_of(&blocks);
        assert!(text.contains("sequenceDiagram"), "{text}");
    }

    fn text_of(blocks: &[Block]) -> String {
        blocks
            .iter()
            .filter_map(|block| match block {
                Block::Prose(lines) => Some(lines),
                Block::Figure(_) => None,
            })
            .flatten()
            .flat_map(|line| line.iter().map(|span| span.text.clone()))
            .collect()
    }

    #[test]
    fn an_unterminated_fence_keeps_its_source_in_the_document() {
        let blocks = blocks("intro\n\n```mermaid\nflowchart LR\n  a --> b\n", 80);
        let text = text_of(&blocks);
        assert!(text.contains("intro"), "{text}");
        assert!(text.contains("flowchart LR"), "{text}");
    }

    #[test]
    fn figure_count_sees_every_diagram() {
        assert_eq!(figure_count(BODY), 1);
        assert_eq!(figure_count("no diagrams here"), 0);
    }

    #[test]
    fn validate_reports_what_it_simplified() {
        let (figures, notes) =
            validate("```mermaid\nflowchart LR\n  a{choose} --> b\n  style a fill:#f00\n```\n");
        assert_eq!(figures, 1);
        assert_eq!(
            notes,
            [
                "diagram 1: `{ }` shapes are drawn as boxes",
                "diagram 1: `style` is ignored",
            ]
        );
    }

    /// A diagram we cannot draw is the one case that reads as a failure, and
    /// the note has to say what to do instead.
    #[test]
    fn validate_names_a_diagram_it_cannot_draw() {
        let (figures, notes) = validate("```mermaid\nsequenceDiagram\n  a->>b: hi\n```\n");
        assert_eq!(figures, 0);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("flowchart LR"), "{notes:?}");
        assert!(notes[0].contains("left as source"), "{notes:?}");
    }

    /// A five-node `flowchart LR` with long labels overflows a narrow card
    /// laid out left to right, so it is redrawn top-down instead, and every
    /// node then fits the width it was given.
    #[test]
    fn a_wide_flowchart_redraws_top_down_to_fit_a_narrow_card() {
        let body = "\
```mermaid
flowchart LR
  a[routers/ai_visualization.py<br/>create, edit, perspective] --> b[second stage of the pipeline]
  b --> c[third stage of the pipeline]
  c --> d[fourth stage of the pipeline]
  d --> e[fifth stage of the pipeline]
```
";
        let card_width = 40;
        let blocks = blocks(body, card_width);
        let Some(Block::Figure(figure)) = blocks.first() else {
            panic!("a figure: {blocks:?}");
        };
        assert_eq!(figure.fit, Fit::Redrawn);
        assert!(
            figure.view.width() <= u16::try_from(card_width).expect("fits u16"),
            "{}",
            figure.view.width()
        );
    }

    /// A figure that already fits keeps the direction its author asked for.
    #[test]
    fn a_flowchart_that_already_fits_keeps_its_declared_direction() {
        let body = "```mermaid\nflowchart LR\n  a[a] --> b[b]\n```\n";
        let blocks = blocks(body, 80);
        let Some(Block::Figure(figure)) = blocks.first() else {
            panic!("a figure: {blocks:?}");
        };
        assert_eq!(figure.fit, Fit::AsDrawn);
    }

    /// The reader is told which of the two happened: a missing file reads
    /// differently from a file that is there but has lost the anchor.
    #[test]
    fn the_two_unresolved_reasons_read_differently() {
        let missing =
            unresolved_explanation("a.rs", Located::FileMissing).expect("explains a missing file");
        let lost = unresolved_explanation("a.rs", Located::Lost).expect("explains a gone anchor");
        assert_ne!(missing, lost);
    }

    /// `Found` and `Whole` are not failures: nothing to explain.
    #[test]
    fn a_resolved_anchor_has_no_explanation() {
        assert_eq!(
            unresolved_explanation("a.rs", Located::Found { line: 1, end: 1 }),
            None
        );
        assert_eq!(unresolved_explanation("a.rs", Located::Whole), None);
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use crate::app::Pane;
    use crate::app::composer::ComposerKind;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{Fixture, key, seat_walkthrough, standard_fixture};

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle(key(c));
        }
    }

    const DIAGRAM: &str = "\
intro

```mermaid
flowchart LR
  a[answer] --> b[caller] --> c[no anchor]
  click a \"src/lib.rs#answer\"
  click b \"src/lib.rs#gone\"
```
";

    fn app_with_walkthrough(fixture: &Fixture) -> App {
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        seat_walkthrough(
            &mut app,
            "How the answer moved",
            &[
                ("The answer", Some("src/lib.rs#answer"), DIAGRAM),
                ("What is left", None, "nothing to point at"),
            ],
        );
        // the figure cache rides with the open diff, whose cards read it
        app.open_walkthrough_diff("w1");
        app
    }

    /// The anchor a stop resolved to, as the fields every comment carries.
    fn stop_anchor(app: &App, index: usize) -> (Option<u32>, Option<u32>) {
        let comment = app.stop_comment(index).expect("a stop comment");
        (comment.anchor.line, comment.anchor.line_end)
    }

    /// Resolve the anchors the way the worker would: `read_rev` when the
    /// walkthrough was published with one, else the live worktree.
    fn resolve(app: &mut App) {
        let Some(request) = app.pending_walkthrough.take() else {
            return;
        };
        let root = app.review.repo_root.clone();
        let read = diffler_core::review::Review::compute_walkthrough_files(
            &root,
            request.read_rev.as_deref(),
            &request.files,
        );
        app.handle(AppEvent::WalkthroughAnchors {
            contents: read.contents,
            pin_broken: read.pin_broken,
            token: request.token,
        });
    }

    /// The point of the symbol anchor: the stop finds its definition's whole
    /// span, and the reader lands on the segment rather than a line. It lands
    /// in the comment's own anchor fields, so the review's outdated detection
    /// and card placement need to know nothing about walkthroughs.
    #[test]
    fn a_stops_symbol_anchor_resolves_into_its_comments_anchor() {
        let fixture = standard_fixture();
        let mut app = app_with_walkthrough(&fixture);
        resolve(&mut app);
        assert_eq!(stop_anchor(&app, 0), (Some(1), Some(3)));
        assert_eq!(
            app.stop_comment(0)
                .expect("a stop")
                .anchor
                .line_text
                .as_deref(),
            Some("}"),
            "the end line's text, the one drift is judged on"
        );
        assert_eq!(stop_anchor(&app, 1), (None, None), "nowhere to land");
    }

    /// One read serves both: the stop's own anchor and the nodes of the
    /// figures in its body.
    #[test]
    fn figure_nodes_resolve_alongside_the_stop_and_stale_ones_are_counted() {
        let fixture = standard_fixture();
        let mut app = app_with_walkthrough(&fixture);
        resolve(&mut app);
        let diff = app.diff.as_ref().expect("diff view");
        let Some(Block::Figure(figure)) = diff
            .figures
            .get("stop-0")
            .expect("the diagram is cached")
            .blocks
            .iter()
            .find(|block| matches!(block, Block::Figure(_)))
        else {
            panic!("a figure");
        };
        assert_eq!(
            figure.resolved[&NodeId::new("a")],
            Located::Found { line: 1, end: 3 }
        );
        assert_eq!(figure.resolved[&NodeId::new("b")], Located::Lost);
        assert_eq!(figure.stale(), 1);
    }

    /// The row of the stop's card holding a figure, for a test that has to
    /// seat the cursor there before opening it.
    fn figure_row(app: &App) -> usize {
        let diff = app.diff.as_ref().expect("diff view");
        let session = app.review.session_for(&diff.source);
        diff.rows()
            .iter()
            .position(|row| match *row {
                DiffRow::Comment { comment, line, .. } => {
                    let Some(comment) = session.comments.get(comment) else {
                        return false;
                    };
                    let blocks = blocks_of(&diff.figures, &comment.id);
                    let unresolved = diff.unresolved_anchors.get(&comment.id).copied();
                    matches!(
                        comment_display(comment, diff.wrap_width, None, blocks, unresolved)
                            .get(line),
                        Some(CommentLine::Figure { .. })
                    )
                }
                _ => false,
            })
            .expect("a figure row in the stop's card")
    }

    /// `o` opens the figure under the cursor full-screen on the Graph screen
    /// carrying its own nodes, with the default selection a card clears
    /// restored; back returns to the same slide and cursor row, since
    /// opening it never touches the diff view's own state.
    #[test]
    fn o_opens_the_figures_graph_and_back_returns_to_the_same_slide_and_cursor() {
        let fixture = standard_fixture();
        let mut app = app_with_walkthrough(&fixture);
        resolve(&mut app);
        app.seat_stop(0);

        let cursor_before = figure_row(&app);
        app.diff.as_mut().expect("diff view").cursor = cursor_before;
        let slide_before = app.diff.as_ref().expect("diff view").slide.clone();

        app.open_figure_graph_at_cursor();

        assert_eq!(app.screen(), Screen::Graph);
        let node_ids: Vec<String> = app
            .graph
            .as_ref()
            .expect("graph view")
            .model()
            .nodes
            .iter()
            .map(|n| n.id.0.clone())
            .collect();
        assert_eq!(node_ids, ["a", "b", "c"], "the figure's own nodes");
        assert!(
            app.graph.as_ref().expect("graph view").selected().is_some(),
            "the default selection is restored"
        );
        assert!(
            app.figure_graph_anchors
                .as_ref()
                .expect("anchors carried over")
                .contains_key(&NodeId::new("a")),
            "a's resolved click anchor carries over"
        );

        app.pop_screen();

        assert_eq!(app.screen(), Screen::Diff);
        let diff = app.diff.as_ref().expect("diff view");
        assert_eq!(diff.cursor, cursor_before, "the cursor row is unchanged");
        assert_eq!(diff.slide, slide_before, "the same slide is still open");
    }

    /// `<cr>` on a node with a resolved `click` anchor seats the reader on
    /// that file and line, the same jump a stop's own anchor gets; a node
    /// with no anchor does nothing.
    #[test]
    fn cr_on_an_anchored_node_jumps_to_its_code_and_an_unanchored_one_does_nothing() {
        let fixture = standard_fixture();
        let mut app = app_with_walkthrough(&fixture);
        resolve(&mut app);
        app.seat_stop(0);
        app.diff.as_mut().expect("diff view").cursor = figure_row(&app);

        app.open_figure_graph_at_cursor();
        app.on_graph_action(&crate::graph::GraphAction::Activated(NodeId::new("a")));

        let request = app.pending_file.as_ref().expect("a queued file open");
        assert_eq!(request.path, "src/lib.rs");
        assert_eq!(request.span, Some((1, 3)));

        app.pending_file = None;
        app.on_graph_action(&crate::graph::GraphAction::Activated(NodeId::new("c")));
        assert!(
            app.pending_file.is_none(),
            "c has no anchor, so <cr> does nothing"
        );
    }

    /// Resolution lands after the reader is already standing on a stop, so
    /// the answer has to reach the open view: the span appears where they
    /// are, rather than on the next visit.
    #[test]
    fn anchors_landing_seat_the_stop_the_reader_already_stands_on() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        seat_walkthrough(
            &mut app,
            "How the answer moved",
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        app.open_walkthrough_diff("w1");
        assert!(
            app.diff
                .as_ref()
                .expect("diff view")
                .rows()
                .iter()
                .all(|row| !matches!(row, crate::app::DiffRow::Line { .. })),
            "nothing seats before the files are read"
        );

        resolve(&mut app);

        let diff = app.diff.as_ref().expect("diff view");
        let model = diff.model(&app.review);
        let lines: Vec<u32> = (0..diff.rows().len())
            .filter_map(|row| match diff.rows().get(row) {
                Some(crate::app::DiffRow::Line { file, hunk, line }) => {
                    model
                        .files
                        .get(*file)?
                        .hunks
                        .get(*hunk)?
                        .lines
                        .get(*line)?
                        .new_no
                }
                _ => None,
            })
            .collect();
        assert_eq!(lines, [1, 2, 3], "the windowed rows carry the whole span");
    }

    /// A body's figures are parsed once and kept, so drawing a card does not
    /// re-parse a diagram every frame; a rewritten body drops the old parse.
    #[test]
    fn a_figure_is_parsed_once_per_body_and_dropped_when_it_is_rewritten() {
        let fixture = standard_fixture();
        let mut app = app_with_walkthrough(&fixture);
        let before = app.diff.as_ref().expect("diff view").figures["stop-0"].hash;
        app.diff
            .as_mut()
            .expect("diff view")
            .ensure_rows(&app.review);
        assert_eq!(
            app.diff.as_ref().expect("diff view").figures["stop-0"].hash,
            before,
            "an unchanged body is not parsed again"
        );

        let source = app.active_review_source();
        app.review
            .session_for_mut(&source)
            .edit_comment("stop-0", "no diagram any more");
        app.diff.as_mut().expect("diff view").invalidate();
        app.diff
            .as_mut()
            .expect("diff view")
            .ensure_rows(&app.review);
        assert!(
            !app.diff
                .as_ref()
                .expect("diff view")
                .figures
                .contains_key("stop-0"),
            "a body with no diagram left keeps no parse"
        );
    }

    /// An agent revising the walkthrough under a reader has to show, so the
    /// rows are keyed by what identifies one rather than built once.
    #[test]
    fn a_republished_walkthrough_rebuilds_the_rows() {
        let fixture = standard_fixture();
        let mut app = app_with_walkthrough(&fixture);
        assert_eq!(app.active_walkthrough().map(|w| w.stops.len()), Some(2));
        let source = app.active_review_source();
        let session = app.review.session_for_mut(&source);
        session.comments.clear();
        crate::test_support::seat_walkthrough_session(
            session,
            "w1",
            "Take two",
            &[("Only stop", None, "shorter now")],
        );
        // a revision keeps the same id; the later publish time is what marks
        // it as changed under the reader
        if let Some(walkthrough) = session.walkthrough.as_mut() {
            walkthrough.at = 1_700_000_001;
        }
        app.ensure_walkthrough_view();
        assert_eq!(
            app.diff
                .as_ref()
                .and_then(|diff| diff.walkthrough_built.clone()),
            Some(("w1".to_owned(), 1_700_000_001)),
        );
    }

    /// A revision can shrink the walkthrough out from under the reader. A
    /// stop index that no longer exists must not be left dangling: the pane
    /// keeps windowed to a real slide on its own, with no keypress needed.
    #[test]
    fn republishing_with_fewer_stops_clamps_a_dangling_slide() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        seat_walkthrough(
            &mut app,
            "five stops",
            &[
                ("One", None, "a"),
                ("Two", None, "b"),
                ("Three", None, "c"),
                ("Four", None, "d"),
                ("Five", None, "e"),
            ],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);
        app.seat_stop(4);
        assert_eq!(
            app.diff.as_ref().expect("diff view").slide,
            Some(Slide::Stop(4))
        );

        let source = app.active_review_source();
        let session = app.review.session_for_mut(&source);
        session.comments.clear();
        crate::test_support::seat_walkthrough_session(
            session,
            "w1",
            "fewer stops",
            &[("One", None, "a"), ("Two", None, "b"), ("Three", None, "c")],
        );
        if let Some(walkthrough) = session.walkthrough.as_mut() {
            walkthrough.at = 1_700_000_001;
        }
        app.ensure_walkthrough_view();

        assert_eq!(
            app.diff.as_ref().expect("diff view").slide,
            Some(Slide::Stop(2)),
            "clamped to the new last stop, with no keypress"
        );
    }

    /// Deleting the ad hoc comment on screen must not leave the pane
    /// windowed to a comment that no longer exists.
    #[test]
    fn deleting_the_open_ad_hoc_comment_leaves_a_valid_slide() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        seat_walkthrough(
            &mut app,
            "one stop",
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);
        let source = app.active_review_source();
        let human = app
            .review
            .session_for_mut(&source)
            .add_comment(
                diffler_core::session::Anchor {
                    file: "ci.yml".to_owned(),
                    line: Some(1),
                    line_end: None,
                    on_old_side: false,
                    line_text: None,
                },
                "reviewer",
                "why on push?",
            )
            .id
            .clone();
        app.diff.as_mut().expect("diff view").invalidate();
        app.diff
            .as_mut()
            .expect("diff view")
            .ensure_rows(&app.review);
        app.enter_slide_for_comment(&human);
        assert_eq!(
            app.diff.as_ref().expect("diff view").slide,
            Some(Slide::AdHoc(human.clone()))
        );

        assert!(app.delete_comment_by_id(&human));

        assert_eq!(
            app.diff.as_ref().expect("diff view").slide,
            None,
            "the deleted comment's slide falls back rather than dangling"
        );
    }

    /// Every code line the slide shows, as text, read through the model that
    /// also carries the walkthrough's own files, since a stop outside the diff
    /// has nothing to show against otherwise.
    fn slide_line_texts(app: &App) -> Vec<String> {
        let diff = app.diff.as_ref().expect("diff view");
        let model =
            crate::app::DiffView::model_with_context(diff.model(&app.review), &diff.context_files);
        (0..diff.rows().len())
            .filter_map(|row| match diff.rows().get(row) {
                Some(crate::app::DiffRow::Line { file, hunk, line }) => Some(
                    model
                        .files
                        .get(*file)?
                        .hunks
                        .get(*hunk)?
                        .lines
                        .get(*line)?
                        .text
                        .clone(),
                ),
                _ => None,
            })
            .collect()
    }

    /// A stop anchored to a file the working-tree diff does not carry (it is
    /// committed and untouched) still gets a slide: the anchor worker's read
    /// becomes a context file, and the span and card render against it the
    /// same as they would inside the diff.
    #[test]
    fn a_stop_anchored_outside_the_diff_renders_its_span_and_card() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        assert!(
            !app.review
                .model()
                .files
                .iter()
                .any(|f| f.path == "notes.txt"),
            "notes.txt is committed and never touched again"
        );
        seat_walkthrough(
            &mut app,
            "an untouched file",
            &[("Notes", Some("notes.txt:1"), "why alpha")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);

        assert!(
            app.diff
                .as_ref()
                .expect("diff view")
                .context_files
                .iter()
                .any(|f| f.path == "notes.txt"),
            "the worker's read became a context file"
        );
        assert_eq!(slide_line_texts(&app), ["alpha"]);
        assert!(
            app.diff
                .as_ref()
                .expect("diff view")
                .rows()
                .iter()
                .any(|row| matches!(row, crate::app::DiffRow::Comment { .. })),
            "the stop's card is in the slide too"
        );

        app.queue_enrich_selected();
        app.enrich_now();
        let diff = app.diff.as_ref().expect("diff view");
        let file = diff
            .context_files
            .iter()
            .find(|f| f.path == "notes.txt")
            .expect("context file");
        assert!(
            diff.highlights
                .get("notes.txt")
                .is_some_and(|cached| cached.hash == file.sides_hash()),
            "a context file highlights like a file the diff carries"
        );
    }

    /// A refresh recomputes the review's own model; the walkthrough's context
    /// files live on the view itself, so one over an unrelated file leaves
    /// them in place.
    #[test]
    fn a_refresh_does_not_drop_the_context_file() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        seat_walkthrough(
            &mut app,
            "an untouched file",
            &[("Notes", Some("notes.txt:1"), "why alpha")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);
        assert!(
            app.diff
                .as_ref()
                .expect("diff view")
                .context_files
                .iter()
                .any(|f| f.path == "notes.txt")
        );

        fixture.write("todo.md", "- [ ] more\n");
        app.queue_refresh();
        app.settle_refresh();

        assert!(
            app.diff
                .as_ref()
                .expect("diff view")
                .context_files
                .iter()
                .any(|f| f.path == "notes.txt"),
            "a refresh over another file leaves the context file in place"
        );
    }

    /// `open_walkthrough` over a clean working tree installs the diff view
    /// anyway (its own file fills the pane once anchors resolve) and seats the
    /// requested stop with no error; the span shows once the read lands.
    #[test]
    fn a_clean_tree_walkthrough_opens_on_the_first_stop_and_shows_it_once_anchors_land() {
        let fixture = Fixture::new();
        fixture.write("README.md", "hello\n");
        fixture.commit_all("initial commit");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        assert!(
            app.review.model().files.is_empty(),
            "a clean tree carries nothing"
        );
        seat_walkthrough(
            &mut app,
            "committed file tour",
            &[("Base", Some("README.md:1"), "why hello")],
        );

        app.open_walkthrough("w1", Slide::Stop(0));

        let diff = app
            .diff
            .as_ref()
            .expect("the walkthrough opened despite the clean tree");
        assert_eq!(diff.layout, crate::config::FileLayout::Walkthrough);
        assert_eq!(diff.slide, Some(Slide::Stop(0)));
        assert_eq!(
            diff.referenced, None,
            "nothing seats before the anchor resolves"
        );
        assert!(app.message.is_none(), "a clean tree is not an error here");

        resolve(&mut app);

        assert_eq!(slide_line_texts(&app), ["hello"]);
    }

    /// A walkthrough pinned to the revision it was published against still
    /// shows a stop's code once the checkout has moved past that revision
    /// and the file is gone from the worktree entirely: the anchor worker
    /// reads the pinned revision instead of coming up with nothing.
    #[test]
    fn a_stop_pinned_to_a_gone_revision_still_shows_its_code() {
        let fixture = Fixture::new();
        fixture.write("gone.rs", "pub fn answer() -> u32 {\n    42\n}\n");
        fixture.commit_all("add gone.rs");
        let pinned = fixture.review().vcs.resolve("HEAD").expect("resolve head");
        fixture.remove_and_commit("gone.rs", "drop gone.rs");

        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        seat_walkthrough(
            &mut app,
            "before the file left",
            &[("The answer", Some("gone.rs#answer"), "why 42")],
        );
        let source = diffler_core::source::ReviewSource::walkthrough("w1");
        if let Some(walkthrough) = app.review.session_for_mut(&source).walkthrough.as_mut() {
            walkthrough.rev = Some(pinned);
        }
        app.review.save_for(&source).expect("save walkthrough");

        app.open_walkthrough_diff("w1");
        resolve(&mut app);

        assert_eq!(
            slide_line_texts(&app),
            ["pub fn answer() -> u32 {", "    42", "}"]
        );
    }

    /// A stop whose file is there but whose symbol is gone still renders its
    /// card, never an empty pane, with the explanation as one more line.
    #[test]
    fn a_stop_whose_symbol_is_gone_still_shows_its_card_and_says_why() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        seat_walkthrough(
            &mut app,
            "a symbol that moved on",
            &[("Gone", Some("src/lib.rs#does_not_exist"), "why")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);

        let diff = app.diff.as_ref().expect("diff view");
        assert!(
            diff.rows()
                .iter()
                .any(|row| matches!(row, DiffRow::Comment { .. })),
            "the card renders even though the anchor never resolved"
        );
        let session = app.review.session_for(&diff.source);
        let comment = session
            .comments
            .iter()
            .find(|c| c.title.as_deref() == Some("Gone"))
            .expect("the stop");
        let unresolved = diff.unresolved_anchors.get(&comment.id).copied();
        assert_eq!(unresolved, Some(Located::Lost), "file present, symbol gone");
        let blocks = blocks_of(&diff.figures, &comment.id);
        let lines = comment_display(comment, diff.wrap_width, None, blocks, unresolved);
        assert!(
            lines
                .iter()
                .any(|line| matches!(line, CommentLine::Note(_))),
            "the explanation renders as a line of its own: {lines:?}"
        );
    }

    /// A stop whose file is not reachable at all (no pinned revision has it,
    /// and neither does the worktree) still renders its card, with an
    /// explanation distinct from a gone symbol's.
    #[test]
    fn a_stop_whose_file_is_missing_entirely_still_shows_its_card_and_says_why() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        seat_walkthrough(
            &mut app,
            "a file that never existed here",
            &[("Nowhere", Some("nowhere.rs:1"), "why")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);

        let diff = app.diff.as_ref().expect("diff view");
        assert!(
            diff.rows()
                .iter()
                .any(|row| matches!(row, DiffRow::Comment { .. })),
            "the card renders even though the file was never found"
        );
        let session = app.review.session_for(&diff.source);
        let comment = session
            .comments
            .iter()
            .find(|c| c.title.as_deref() == Some("Nowhere"))
            .expect("the stop");
        let unresolved = diff.unresolved_anchors.get(&comment.id).copied();
        assert_eq!(
            unresolved,
            Some(Located::FileMissing),
            "no file to read at all"
        );
    }

    /// Land the diff cursor on the first code line the open slide shows.
    fn cursor_to_slide_line(app: &mut App) {
        let diff = app.diff.as_mut().expect("diff view");
        diff.focus = Pane::Diff;
        let position = diff
            .rows()
            .iter()
            .position(|row| matches!(row, DiffRow::Line { .. }))
            .expect("the slide shows a code line");
        diff.cursor = position;
    }

    /// `c` on a code line inside a stop's slide is the reader answering the
    /// stop about that code: the composer opens anchored to the slide's own
    /// file and line, not "no file under the cursor", and the saved comment
    /// carries them into the walkthrough's own review.
    #[test]
    fn c_on_a_slide_line_opens_a_composer_anchored_there_and_saves_the_comment() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        seat_walkthrough(
            &mut app,
            "notes tour",
            &[("Notes", Some("notes.txt:1"), "why alpha")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);
        cursor_to_slide_line(&mut app);

        app.handle(key('c'));
        let composer = app
            .diff
            .as_ref()
            .and_then(|d| d.composer.clone())
            .expect("c opens a composer");
        let ComposerKind::New { anchor } = composer.kind else {
            panic!("expected a new-comment composer, got {:?}", composer.kind);
        };
        assert_eq!(anchor.file, "notes.txt");
        assert_eq!(anchor.line, Some(1));

        type_text(&mut app, "why not beta");
        app.handle(key('\n'));

        let source = diffler_core::source::ReviewSource::walkthrough("w1");
        let comment = app
            .review
            .session_for(&source)
            .comments
            .iter()
            .find(|c| c.body == "why not beta")
            .expect("the reply landed in the walkthrough's own review");
        assert_eq!(comment.anchor.file, "notes.txt");
        assert_eq!(comment.anchor.line, Some(1));
    }

    /// `V` then `c` inside a slide anchors the visual range, the same as it
    /// would in an ordinary diff.
    #[test]
    fn capital_v_then_c_on_a_slide_anchors_the_selected_range() {
        let fixture = Fixture::new();
        fixture.write("notes.txt", "alpha\nbeta\ngamma\n");
        fixture.commit_all("add notes");
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        seat_walkthrough(
            &mut app,
            "notes tour",
            &[("Notes", Some("notes.txt:1-3"), "why")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);
        cursor_to_slide_line(&mut app);

        app.handle(key('V'));
        app.handle(key('j'));
        app.handle(key('j'));
        app.handle(key('c'));
        type_text(&mut app, "the whole block");
        app.handle(key('\n'));

        let source = diffler_core::source::ReviewSource::walkthrough("w1");
        let comment = app
            .review
            .session_for(&source)
            .comments
            .iter()
            .find(|c| c.body == "the whole block")
            .expect("the range comment landed");
        assert_eq!(comment.anchor.file, "notes.txt");
        assert_eq!(comment.anchor.line, Some(1));
        assert_eq!(comment.anchor.line_end, Some(3));
    }

    /// `e` on a slide line requests the editor at that file and line, the
    /// same jump it makes anywhere else in the diff.
    #[test]
    fn e_on_a_slide_line_requests_the_editor_for_that_file_and_line() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.config.editor.command = Some("vim".to_owned());
        seat_walkthrough(
            &mut app,
            "notes tour",
            &[("Notes", Some("notes.txt:1"), "why alpha")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);
        cursor_to_slide_line(&mut app);

        app.handle(key('e'));

        let request = app.pending_editor.clone().expect("editor request");
        assert_eq!(
            request.purpose,
            crate::editor::EditorPurpose::OpenFile {
                path: "notes.txt".to_owned(),
            }
        );
        let absolute = fixture.root.join("notes.txt");
        assert_eq!(
            request.cmd,
            vec![
                "vim".to_owned(),
                "+1".to_owned(),
                absolute.to_string_lossy().into_owned(),
            ]
        );
    }

    /// A walkthrough is pinned to the revision it was published against, so
    /// a stop can point at code the worktree no longer carries at all. `e`
    /// there has nothing to edit, and says so instead of handing the editor
    /// an empty buffer.
    #[test]
    fn e_on_a_slide_line_whose_file_left_the_worktree_says_so_and_opens_nothing() {
        let fixture = Fixture::new();
        fixture.write("gone.rs", "pub fn answer() -> u32 {\n    42\n}\n");
        fixture.commit_all("add gone.rs");
        let pinned = fixture.review().vcs.resolve("HEAD").expect("resolve head");
        fixture.remove_and_commit("gone.rs", "drop gone.rs");

        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.config.editor.command = Some("vim".to_owned());
        seat_walkthrough(
            &mut app,
            "before the file left",
            &[("The answer", Some("gone.rs#answer"), "why 42")],
        );
        let source = diffler_core::source::ReviewSource::walkthrough("w1");
        if let Some(walkthrough) = app.review.session_for_mut(&source).walkthrough.as_mut() {
            walkthrough.rev = Some(pinned);
        }
        app.review.save_for(&source).expect("save walkthrough");

        app.open_walkthrough_diff("w1");
        resolve(&mut app);
        cursor_to_slide_line(&mut app);

        app.handle(key('e'));

        assert!(
            app.pending_editor.is_none(),
            "no editor for a file the worktree no longer has"
        );
        let message = app
            .message
            .as_ref()
            .expect("a refusal message")
            .text
            .clone();
        assert!(
            message.contains("gone.rs") && message.contains("working tree"),
            "{message}"
        );
    }

    /// A comment made on a slide line is not a detour into some other view:
    /// it renders in the same slide once the card is saved.
    #[test]
    fn a_comment_made_on_a_slide_line_shows_in_the_slide_afterwards() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        seat_walkthrough(
            &mut app,
            "notes tour",
            &[("Notes", Some("notes.txt:1"), "why alpha")],
        );
        app.open_walkthrough_diff("w1");
        resolve(&mut app);
        cursor_to_slide_line(&mut app);

        app.handle(key('c'));
        type_text(&mut app, "why not beta");
        app.handle(key('\n'));

        let review = &app.review;
        let diff = app.diff.as_mut().expect("diff view");
        diff.ensure_rows(review);
        assert!(
            diff.rows()
                .iter()
                .any(|row| matches!(row, DiffRow::Comment { .. })),
            "the new comment renders in the slide"
        );
    }
}
