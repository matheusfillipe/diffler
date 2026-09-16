//! App-side handling of agent tool calls. Runs synchronously on the main
//! loop against the owned review state; the `mcp` module only ships
//! requests here and renders the responses.

use std::collections::{HashMap, HashSet};

use diffler_core::model::DiffModel;
use diffler_core::session::{Anchor, Comment, CommentStatus, now_unix};
use diffler_core::source::ReviewSource;
use diffler_core::walkthrough::{
    BODY_MAX_BYTES, MAX_STOPS, Receipt, ReceiptCode, TOTAL_MAX_BYTES, Target, Walkthrough,
};

use super::App;
use crate::mcp::{
    AGENT_AUTHOR, CommentInfo, FileEntry, McpRequestKind, McpResponse, NoteInfo, NoteParams,
    ReceiptInfo, ReviewStatusResponse, ReviewSummary, StopInfo, StopParams, WalkthroughInfo,
    WalkthroughPublished, WalkthroughSummary, comment_info, comment_status_name, file_status_name,
    render_unified,
};

/// The [`ReviewSource`] variants whose diffs are computed once and cached;
/// `WorkingTree` and `Against` always read live and never reach
/// [`App::source_model`]'s cache path.
enum CachedKind<'a> {
    Commit(&'a str),
    Range(&'a str, &'a str),
    Pr(u64),
}

impl App {
    pub(crate) fn handle_mcp(&mut self, kind: McpRequestKind) -> McpResponse {
        match kind {
            McpRequestKind::ReviewStatus => McpResponse::Status(self.review_status_response()),
            McpRequestKind::GetDiff { file } => {
                match render_unified(self.review.model(), file.as_deref()) {
                    Ok(diff) => McpResponse::Diff(diff),
                    Err(message) => McpResponse::Error(message),
                }
            }
            McpRequestKind::GetComments { status } => McpResponse::Comments(
                self.comments_response(|c| status.is_none_or(|wanted| c == wanted)),
            ),
            McpRequestKind::ListReviews => McpResponse::Reviews(self.review_summaries()),
            McpRequestKind::ReplyComment { id, body } => self.agent_reply(&id, &body),
            McpRequestKind::ProposeResolve { id, note } => {
                self.agent_propose_resolve(&id, note.as_deref())
            }
            McpRequestKind::MarkViewed { file } => self.agent_mark_viewed(&file),
            McpRequestKind::Feedback => McpResponse::Feedback {
                comments: self.comments_response(|c| c != CommentStatus::Resolved),
            },
            McpRequestKind::PublishWalkthrough {
                id,
                title,
                stops,
                skipped,
                summary,
            } => self.agent_publish_walkthrough(id, title, &stops, skipped, summary),
            McpRequestKind::GetWalkthrough { id } => match self.walkthrough_info(id.as_deref()) {
                Ok(info) => McpResponse::Walkthrough(info),
                Err(err) => McpResponse::Error(err),
            },
        }
    }

    fn review_status_response(&self) -> ReviewStatusResponse {
        let files_changed = self
            .review
            .model()
            .files
            .iter()
            .map(|f| FileEntry {
                path: f.path.clone(),
                status: file_status_name(f.status).to_owned(),
                viewed: self.review.session.is_viewed(&f.path, &f.content_hash()),
            })
            .collect();
        let (open, replied, resolved) = count_by_status(&self.review.session.comments);
        // named, not just skipped, so an agent knows a walkthrough or review
        // might be missing from the lists below because its file would not
        // parse, rather than reading a clean repository that has none
        let corrupt_reviews = self
            .review
            .all_reviews_and_corrupt()
            .map_or_else(|_| Vec::new(), |(_, corrupt)| corrupt)
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        ReviewStatusResponse {
            repo: self
                .review
                .repo_root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            branch: self.head.branch.clone(),
            oid7: self.head.oid7.clone(),
            files_changed,
            open_comments: open,
            replied_comments: replied,
            resolved_comments: resolved,
            feedback_epoch: self.feedback_epoch(),
            reviews: self.review_summaries(),
            walkthroughs: self.walkthrough_summaries(),
            corrupt_reviews,
        }
    }

    /// Every walkthrough on disk, newest published first.
    fn walkthrough_summaries(&self) -> Vec<WalkthroughSummary> {
        let mut rows: Vec<WalkthroughSummary> = self
            .review
            .all_reviews()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(source, session)| {
                let ReviewSource::Walkthrough { id } = source else {
                    return None;
                };
                let walkthrough = session.walkthrough?;
                Some(WalkthroughSummary {
                    id,
                    title: walkthrough.title,
                    stops: walkthrough.stops.len(),
                    at: walkthrough.at,
                })
            })
            .collect();
        rows.sort_by_key(|w| std::cmp::Reverse(w.at));
        rows
    }

    /// The walkthrough `id` names, or the newest one on disk when `id` is
    /// `None`. `Ok(None)` means no such walkthrough exists; a genuine read
    /// failure (a corrupt review file) is `Err`, so the two are never folded
    /// into the same answer.
    fn walkthrough_info(&mut self, id: Option<&str>) -> Result<Option<WalkthroughInfo>, String> {
        let id = match id {
            Some(id) => id.to_owned(),
            None => match self.walkthrough_summaries().into_iter().next() {
                Some(summary) => summary.id,
                None => return Ok(None),
            },
        };
        let source = ReviewSource::Walkthrough { id };
        self.review
            .ensure_source(&source)
            .map_err(|err| err.to_string())?;
        let session = self.review.session_for(&source);
        let Some(walkthrough) = session.walkthrough.as_ref() else {
            return Ok(None);
        };
        Ok(Some(WalkthroughInfo {
            id: walkthrough.id.clone(),
            title: walkthrough.title.clone(),
            author: walkthrough.author.clone(),
            at: walkthrough.at,
            skipped: walkthrough.skipped.clone(),
            summary: walkthrough.summary.clone(),
            rev: walkthrough.rev.clone(),
            stops: walkthrough
                .stops
                .iter()
                .zip(walkthrough.notes_by_stop(&session.comments))
                .filter_map(|(id, notes)| {
                    let comment = session.comment(id)?;
                    Some(StopInfo {
                        id: comment.id.clone(),
                        title: comment.title.clone().unwrap_or_default(),
                        anchor: comment.anchor_ref.clone(),
                        body: comment.body.clone(),
                        notes: notes
                            .iter()
                            .filter_map(|id| {
                                let note = session.comment(id)?;
                                Some(NoteInfo {
                                    id: note.id.clone(),
                                    anchor: note.anchor_ref.clone(),
                                    body: note.body.clone(),
                                })
                            })
                            .collect(),
                    })
                })
                .collect(),
        }))
    }

    /// Store the agent's reading order as its own review source: revises the
    /// walkthrough `id` names, or creates a new one when `id` is `None`.
    /// Every stop becomes an agent comment, so the human answers it in its
    /// own thread; a stop that passes its comment id back keeps that thread,
    /// and every other agent comment the source held goes with it (a human
    /// comment or reply is never one of these, so it always survives).
    /// Anchor and figure receipts are reported but never refuse: the
    /// walkthrough is stored either way, so the agent learns what to fix
    /// without a broken figure ever reaching the reader.
    fn agent_publish_walkthrough(
        &mut self,
        id: Option<String>,
        title: String,
        stops: &[StopParams],
        skipped: Option<String>,
        summary: Option<String>,
    ) -> McpResponse {
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let source = ReviewSource::Walkthrough { id: id.clone() };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        // every publish, a revision included, redescribes the stops against
        // whatever is checked out right now, so it is pinned to that HEAD
        // and not whatever an earlier revision of this same walkthrough
        // named; a repo with no commits yet resolves to nothing, the same as
        // a walkthrough saved before `rev` existed
        let rev = self.review.vcs.resolve("HEAD").ok();
        // a stop with no anchor still needs a file to hang its card on: the
        // first one the walkthrough names, else the first file of the
        // working tree, which a walkthrough always tracks
        let model = self.review.model().clone();
        let fallback = stops
            .iter()
            .filter_map(|stop| stop.anchor.as_deref())
            .map(|anchor| Target::parse(anchor).path().to_owned())
            .next()
            .or_else(|| model.files.first().map(|file| file.path.clone()))
            .unwrap_or_default();
        let files: Vec<String> = stops
            .iter()
            .map(|stop| anchored_path(stop.anchor.as_deref(), &fallback))
            .collect();

        let refusals = walkthrough_refusals(stops, &files, summary.as_deref());
        if !refusals.is_empty() {
            let text = refusals
                .iter()
                .map(|receipt| {
                    let stop = receipt
                        .stop
                        .map_or("-".to_owned(), |index| index.to_string());
                    format!("stop {stop}: {}: {}", receipt.code.name(), receipt.detail)
                })
                .collect::<Vec<_>>()
                .join("\n");
            return McpResponse::Error(text);
        }

        let receipts = walkthrough_receipts(stops);
        let kept: HashSet<&str> = stops
            .iter()
            .flat_map(|stop| {
                std::iter::once(stop.id.as_deref())
                    .chain(notes_of(stop).map(|note| note.id.as_deref()))
            })
            .flatten()
            .collect();
        let session = self.review.session_for_mut(&source);
        // every agent comment this source already held is this walkthrough's
        // previous revision; a fresh source has none, so nothing to prune
        let previous: Vec<String> = session
            .comments
            .iter()
            .filter(|c| c.author == AGENT_AUTHOR)
            .map(|c| c.id.clone())
            .collect();
        for id in &previous {
            if !kept.contains(id.as_str()) {
                session.delete_comment(id);
            }
        }

        let mut stop_ids = Vec::with_capacity(stops.len());
        for (stop, file) in stops.iter().zip(&files) {
            let id = write_agent_comment(
                session,
                stop.id.as_deref(),
                Some(stop.title.clone()),
                stop.anchor.clone(),
                file,
                &stop.body,
            );
            stop_ids.push(id);
            for note in notes_of(stop) {
                // a note with no anchor of its own rides the stop's, and the
                // worker seats it where that region starts
                let anchor = note.anchor.clone().or_else(|| stop.anchor.clone());
                write_agent_comment(session, note.id.as_deref(), None, anchor, file, &note.body);
            }
        }

        let count = stop_ids.len();
        session.set_walkthrough(Walkthrough {
            id: id.clone(),
            title,
            author: AGENT_AUTHOR.to_owned(),
            at: diffler_core::session::now_unix_millis(),
            stops: stop_ids,
            skipped,
            summary,
            rev: rev.clone(),
        });
        if let Err(err) = self.persist_review_change(&source) {
            return McpResponse::Error(format!("walkthrough not saved: {err}"));
        }
        self.ensure_walkthrough_view();
        self.queue_walkthrough_anchors();
        self.reload_walkthroughs();
        self.info("agent published a walkthrough");

        McpResponse::WalkthroughPublished(WalkthroughPublished {
            id,
            stops: count,
            receipts,
            rev,
        })
    }

    fn review_summaries(&self) -> Vec<ReviewSummary> {
        self.review
            .all_reviews()
            .unwrap_or_default()
            .into_iter()
            .map(|(source, session)| {
                let (open, replied, resolved) = count_by_status(&session.comments);
                // a walkthrough's own title reads better than its fallback
                // `walkthrough <id8>` label, once the session is loaded
                let label = session
                    .walkthrough
                    .as_ref()
                    .map_or_else(|| source.label(), |w| w.title.clone());
                ReviewSummary {
                    source: source.key(),
                    label,
                    open_comments: open,
                    replied_comments: replied,
                    resolved_comments: resolved,
                }
            })
            .collect()
    }

    /// The diff a source is reviewing, used to render comment context and judge
    /// outdated-ness. Commit and range diffs are immutable, so they compute
    /// once and stay cached: agent polls must not stall the render loop.
    /// Backend errors degrade to an empty diff.
    pub(crate) fn source_model(&mut self, source: &ReviewSource) -> std::sync::Arc<DiffModel> {
        // narrowing to the cacheable sources up front makes WorkingTree
        // structurally absent below, instead of an unreachable match arm
        let kind = match source {
            // a walkthrough always tracks the working tree, the same as
            // `WorkingTree` itself, never pinned to a rev
            ReviewSource::WorkingTree | ReviewSource::Walkthrough { .. } => {
                return std::sync::Arc::new(self.review.model().clone());
            }
            // live like the working tree, so caching it would go stale; the
            // open view already holds a model the refresh keeps current
            ReviewSource::Against { rev } => {
                return std::sync::Arc::new(self.against_model_for(rev));
            }
            ReviewSource::Commit { oid } => CachedKind::Commit(oid),
            ReviewSource::Range { oldest, newest } => CachedKind::Range(oldest, newest),
            ReviewSource::Pr { number } => CachedKind::Pr(*number),
        };
        let key = source.key();
        if !self.source_models.contains_key(&key) {
            let model = match kind {
                CachedKind::Commit(oid) => self.review.vcs.commit_diff(oid).unwrap_or_default(),
                CachedKind::Range(oldest, newest) => self
                    .review
                    .vcs
                    .range_diff(oldest, newest)
                    .unwrap_or_default(),
                // resolved when the PR view opened; unknown PRs degrade empty
                CachedKind::Pr(number) => self
                    .pr_ranges
                    .get(&number)
                    .and_then(|(base, head)| self.review.vcs.tree_diff(base, head).ok())
                    .unwrap_or_default(),
            };
            self.source_models
                .insert(key.clone(), std::sync::Arc::new(model));
        }
        self.source_models.get(&key).cloned().unwrap_or_default()
    }

    /// Comments across every review, each tagged with its source so the agent
    /// knows what the human reviewed and where the change came from.
    fn comments_response(&mut self, keep: impl Fn(CommentStatus) -> bool) -> Vec<CommentInfo> {
        let mut out = Vec::new();
        for (source, session) in self.review.all_reviews().unwrap_or_default() {
            let comments: Vec<_> = session.comments.iter().filter(|c| keep(c.status)).collect();
            // a live source rebuilds its model here, so an agent poll must not
            // pay for one whose comments it is about to discard
            if comments.is_empty() {
                continue;
            }
            let model = self.source_model(&source);
            for comment in comments {
                out.push(comment_info(comment, &model, &source));
            }
        }
        out
    }

    /// The review a comment id lives in, searching every persisted source.
    fn source_of_comment(&self, id: &str) -> Option<ReviewSource> {
        self.review
            .all_reviews()
            .ok()?
            .into_iter()
            .find(|(_, session)| session.comments.iter().any(|c| c.id == id))
            .map(|(source, _)| source)
    }

    fn agent_reply(&mut self, id: &str, body: &str) -> McpResponse {
        let Some(source) = self.source_of_comment(id) else {
            return McpResponse::Error(format!("unknown comment id: {id}"));
        };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        self.review
            .session_for_mut(&source)
            .reply(id, AGENT_AUTHOR, body);
        if let Err(err) = self.persist_review_change(&source) {
            return McpResponse::Error(err);
        }
        self.info("agent replied to a comment");
        self.comment_status_response(&source, id)
    }

    /// The agent can only propose: the comment moves to replied and the human
    /// resolves it in the TUI (`R`). The note lands as the reply when the
    /// thread is empty, so a flag on an answered comment adds nothing.
    fn agent_propose_resolve(&mut self, id: &str, note: Option<&str>) -> McpResponse {
        let Some(source) = self.source_of_comment(id) else {
            return McpResponse::Error(format!("unknown comment id: {id}"));
        };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        let session = self.review.session_for_mut(&source);
        // the agent's own answer is what a note would restate; a reply from
        // the human or another reviewer says nothing about this flag
        let answered = session.comment(id).is_some_and(|comment| {
            comment
                .replies
                .iter()
                .any(|reply| reply.author == AGENT_AUTHOR)
        });
        // the note speaks only when the thread is otherwise empty: an agent
        // that replied and then proposed would say the same thing twice
        match note.map(str::trim).filter(|note| !note.is_empty()) {
            Some(note) if !answered => {
                session.reply(id, AGENT_AUTHOR, note);
            }
            _ => {
                session.mark_replied(id);
            }
        }
        if let Err(err) = self.persist_review_change(&source) {
            return McpResponse::Error(err);
        }
        self.info("agent proposed resolving a comment (confirm with R)");
        self.comment_status_response(&source, id)
    }

    fn comment_status_response(&self, source: &ReviewSource, id: &str) -> McpResponse {
        let status = self
            .review
            .session_for(source)
            .comments
            .iter()
            .find(|c| c.id == id)
            .map_or(CommentStatus::Open, |c| c.status);
        McpResponse::Replied {
            status: comment_status_name(status).to_owned(),
        }
    }

    fn agent_mark_viewed(&mut self, file: &str) -> McpResponse {
        // mark the file in the review the human is currently looking at, so a
        // commit/range diff gets its own viewed marks like the working tree
        let source = self.active_review_source();
        let Some(hash) = self
            .source_model(&source)
            .files
            .iter()
            .find(|f| f.path == file)
            .map(diffler_core::model::FileDiff::content_hash)
        else {
            return McpResponse::Error(format!("unknown file: {file}"));
        };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        self.review
            .session_for_mut(&source)
            .mark_viewed(file, &hash);
        if let Err(err) = self.persist_review_change(&source) {
            return McpResponse::Error(err);
        }
        self.info(format!("agent marked {file} viewed"));
        McpResponse::Ok
    }
}

/// The file an anchor names, falling back to the file a walkthrough hangs its
/// anchorless cards on.
fn anchored_path(anchor: Option<&str>, fallback: &str) -> String {
    anchor.map_or_else(
        || fallback.to_owned(),
        |anchor| Target::parse(anchor).path().to_owned(),
    )
}

fn notes_of(stop: &StopParams) -> impl Iterator<Item = &NoteParams> {
    stop.notes.iter().flatten()
}

/// Write one comment the walkthrough owns, reusing the one `id` names so a
/// revision keeps the thread hanging off it. Lines stay unset: the worker
/// resolves `anchor_ref` against the file and fills them in.
fn write_agent_comment(
    session: &mut diffler_core::session::Session,
    id: Option<&str>,
    title: Option<String>,
    anchor_ref: Option<String>,
    file: &str,
    body: &str,
) -> String {
    let anchor = Anchor {
        file: anchored_path(anchor_ref.as_deref(), file),
        line: None,
        line_end: None,
        on_old_side: false,
        line_text: None,
    };
    let existing = id.and_then(|id| session.comments.iter_mut().find(|c| c.id == id));
    if let Some(comment) = existing {
        comment.title = title;
        comment.anchor_ref = anchor_ref;
        comment.anchor = anchor;
        body.clone_into(&mut comment.body);
        return comment.id.clone();
    }
    let id = uuid::Uuid::new_v4().to_string();
    session.comments.push(Comment {
        id: id.clone(),
        author: AGENT_AUTHOR.to_owned(),
        remote_id: None,
        thread_id: None,
        anchor,
        title,
        anchor_ref,
        body: body.to_owned(),
        status: CommentStatus::Open,
        replies: Vec::new(),
        at: now_unix(),
    });
    id
}

/// Every id repeated across `stops` and their notes within one publish. A
/// second write sharing an id with an earlier one in the same call would
/// find the comment the first just made and overwrite its title, anchor and
/// body, so the walkthrough ends up with a duplicate id in its stop list and
/// the first stop or note silently lost.
fn duplicate_id_receipts(stops: &[StopParams]) -> Vec<Receipt> {
    let mut first_seen: HashMap<&str, usize> = HashMap::new();
    let mut receipts = Vec::new();
    for (index, stop) in stops.iter().enumerate() {
        let ids = std::iter::once(stop.id.as_deref())
            .chain(notes_of(stop).map(|note| note.id.as_deref()))
            .flatten();
        for id in ids {
            if let Some(&first) = first_seen.get(id) {
                receipts.push(Receipt {
                    stop: Some(index),
                    code: ReceiptCode::DuplicateId,
                    detail: format!("id \"{id}\" repeats stop {first}'s"),
                });
            } else {
                first_seen.insert(id, index);
            }
        }
    }
    receipts
}

/// Every hard-limit receipt the incoming stops and summary earn. All codes
/// here are refusals: nothing is stored while any are present. `files` is the
/// file each stop lands in, which is the file its notes have to stay inside.
fn walkthrough_refusals(
    stops: &[StopParams],
    files: &[String],
    summary: Option<&str>,
) -> Vec<Receipt> {
    let mut receipts = Vec::new();
    if stops.is_empty() {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::EmptyStops,
            detail: "a walkthrough needs at least one stop".to_owned(),
        });
    } else if stops.len() > MAX_STOPS {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::TooManyStops,
            detail: format!("{} stops, {MAX_STOPS} at most", stops.len()),
        });
    }
    receipts.extend(duplicate_id_receipts(stops));

    let mut total = summary.map_or(0, str::len);
    if let Some(summary) = summary.filter(|summary| summary.len() > BODY_MAX_BYTES) {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::BodyTooLong,
            detail: format!("summary: {} bytes, {BODY_MAX_BYTES} at most", summary.len()),
        });
    }
    for (index, stop) in stops.iter().enumerate() {
        total += stop.body.len();
        for (at, note) in notes_of(stop).enumerate() {
            total += note.body.len();
            if note.body.len() > BODY_MAX_BYTES {
                receipts.push(Receipt {
                    stop: Some(index),
                    code: ReceiptCode::BodyTooLong,
                    detail: format!(
                        "note {at}: {} bytes, {BODY_MAX_BYTES} at most",
                        note.body.len()
                    ),
                });
            }
            let Some(anchor) = note.anchor.as_deref() else {
                continue;
            };
            let path = Target::parse(anchor).path().to_owned();
            if files.get(index).is_some_and(|file| *file != path) {
                receipts.push(Receipt {
                    stop: Some(index),
                    code: ReceiptCode::NoteOutsideStop,
                    detail: format!(
                        "note {at} anchors \"{anchor}\", outside stop {index}'s file {}",
                        files.get(index).map_or("", String::as_str)
                    ),
                });
            }
        }
        if stop.body.len() > BODY_MAX_BYTES {
            receipts.push(Receipt {
                stop: Some(index),
                code: ReceiptCode::BodyTooLong,
                detail: format!("{} bytes, {BODY_MAX_BYTES} at most", stop.body.len()),
            });
        }
        // `Target::parse` never fails, so the only unparsable anchor is one
        // with nothing in it
        if stop.anchor.as_deref().is_some_and(|a| a.trim().is_empty()) {
            receipts.push(Receipt {
                stop: Some(index),
                code: ReceiptCode::AnchorUnparsed,
                detail: "anchor is empty".to_owned(),
            });
        }
    }
    if total > TOTAL_MAX_BYTES {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::TotalTooLong,
            detail: format!("{total} bytes total, {TOTAL_MAX_BYTES} at most"),
        });
    }
    receipts
}

fn count_by_status(comments: &[diffler_core::session::Comment]) -> (usize, usize, usize) {
    let (mut open, mut replied, mut resolved) = (0, 0, 0);
    for comment in comments {
        match comment.status {
            CommentStatus::Open => open += 1,
            CommentStatus::Replied => replied += 1,
            CommentStatus::Resolved => resolved += 1,
        }
    }
    (open, replied, resolved)
}

/// Receipts for a walkthrough that already cleared `validate`: a bare-path
/// anchor highlights the whole file rather than a span, and a stop's own
/// `mermaid` fences may have drawn simplified or, with no shape at all,
/// dropped to their source. Reported so the agent learns without a broken
/// figure ever reaching the reader.
fn walkthrough_receipts(stops: &[StopParams]) -> Vec<ReceiptInfo> {
    let mut receipts = Vec::new();
    for (index, stop) in stops.iter().enumerate() {
        if let Some(anchor) = &stop.anchor
            && matches!(Target::parse(anchor), Target::File { .. })
        {
            receipts.push(ReceiptInfo {
                stop: Some(index),
                code: "anchor_whole".to_owned(),
                detail: format!("\"{anchor}\" has no symbol or line; the whole file anchors"),
            });
        }
        let bodies = std::iter::once(&stop.body).chain(notes_of(stop).map(|note| &note.body));
        for body in bodies {
            let (figures, notes) = crate::app::walkthrough::validate(body);
            let code = if figures == 0 {
                "figure_dropped"
            } else {
                "figure_simplified"
            };
            receipts.extend(notes.into_iter().map(|detail| ReceiptInfo {
                stop: Some(index),
                code: code.to_owned(),
                detail,
            }));
        }
    }
    receipts
}

#[cfg(test)]
mod tests {
    use diffler_core::session::Anchor;

    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    fn app_with_comment() -> (crate::test_support::Fixture, App, String) {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let id = app
            .review
            .session
            .add_comment(
                Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("    42".to_owned()),
                },
                "human",
                "why 42?",
            )
            .id
            .clone();
        (fixture, app, id)
    }

    /// A live source rebuilds its diff whenever the agent asks for comments, so
    /// one contributing nothing must not be built at all. The cache is the
    /// observable: a commit source populates it the moment its model is built.
    #[test]
    fn a_source_contributing_no_comments_builds_no_model() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let oid = app.status.recent[0].oid.clone();
        app.review
            .ensure_source(&ReviewSource::commit(&oid))
            .expect("source");

        assert!(app.comments_response(|_| true).is_empty());
        assert!(
            app.source_models.is_empty(),
            "a commentless source was diffed anyway"
        );

        // and it is built as soon as that source has something to say
        app.review
            .session_for_mut(&ReviewSource::commit(&oid))
            .add_comment(
                Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("    42".to_owned()),
                },
                "human",
                "why 42?",
            );
        assert_eq!(app.comments_response(|_| true).len(), 1);
        assert!(
            app.source_models
                .contains_key(&ReviewSource::commit(&oid).key()),
            "the commented source was diffed"
        );
    }

    #[test]
    fn commit_models_are_computed_once_and_cached() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let oid = app.status.recent[0].oid.clone();
        let source = ReviewSource::commit(&oid);
        let first = app.source_model(&source);
        let second = app.source_model(&source);
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "second lookup reuses the cached model"
        );
    }

    #[test]
    fn review_status_reports_files_counts_and_epoch() {
        let (fixture, mut app, _id) = app_with_comment();
        let McpResponse::Status(status) = app.handle_mcp(McpRequestKind::ReviewStatus) else {
            panic!("expected a status response");
        };
        assert_eq!(
            status.repo,
            fixture.root.file_name().unwrap().to_string_lossy()
        );
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.oid7.len(), 7);
        assert!(status.files_changed.iter().any(|f| f.path == "src/lib.rs"));
        assert!(status.files_changed.iter().all(|f| !f.viewed));
        assert_eq!(status.open_comments, 1);
        assert_eq!(status.replied_comments, 0);
        assert_eq!(status.resolved_comments, 0);
        assert_eq!(status.feedback_epoch, 0);
    }

    #[test]
    fn get_diff_renders_and_rejects_unknown_files() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::Diff(diff) = app.handle_mcp(McpRequestKind::GetDiff { file: None }) else {
            panic!("expected a diff response");
        };
        assert!(diff.contains("+++ b/src/lib.rs"));
        assert!(diff.contains("+    42"));

        let response = app.handle_mcp(McpRequestKind::GetDiff {
            file: Some("nope.rs".to_owned()),
        });
        assert!(matches!(response, McpResponse::Error(message) if message.contains("nope.rs")));
    }

    #[test]
    fn pairing_deferral_does_not_change_mcp_diff_or_feedback() {
        let (_fixture, mut app, _id) = app_with_comment();
        // capture the agent-facing outputs while the model carries no emphasis
        let McpResponse::Diff(before_diff) = app.handle_mcp(McpRequestKind::GetDiff { file: None })
        else {
            panic!("expected a diff response");
        };
        let McpResponse::Feedback {
            comments: before_feedback,
            ..
        } = app.handle_mcp(McpRequestKind::Feedback)
        else {
            panic!("expected comments");
        };

        // enrich the whole working model with intra-line emphasis, the thing
        // the backend used to do eagerly and the TUI now does per file
        for file in &mut app.review.model_mut().files {
            diffler_core::pairing::enrich_file(file);
        }
        let has_emphasis = app
            .review
            .model()
            .files
            .iter()
            .flat_map(|f| &f.hunks)
            .flat_map(|h| &h.lines)
            .any(|l| !l.emphasis.is_empty());
        assert!(has_emphasis, "the fixture has a paired line to emphasize");

        let McpResponse::Diff(after_diff) = app.handle_mcp(McpRequestKind::GetDiff { file: None })
        else {
            panic!("expected a diff response");
        };
        let McpResponse::Feedback {
            comments: after_feedback,
            ..
        } = app.handle_mcp(McpRequestKind::Feedback)
        else {
            panic!("expected comments");
        };
        // emphasis is a render-only concern: MCP output is byte-identical
        assert_eq!(before_diff, after_diff, "get_diff ignores emphasis");
        assert_eq!(before_feedback, after_feedback, "feedback ignores emphasis");
    }

    #[test]
    fn get_comments_filters_by_status() {
        let (_fixture, mut app, id) = app_with_comment();
        let McpResponse::Comments(all) =
            app.handle_mcp(McpRequestKind::GetComments { status: None })
        else {
            panic!("expected comments");
        };
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
        assert_eq!(all[0].context.as_deref(), Some("-    41\n+    42\n }"));
        assert!(!all[0].outdated);

        let McpResponse::Comments(resolved) = app.handle_mcp(McpRequestKind::GetComments {
            status: Some(CommentStatus::Resolved),
        }) else {
            panic!("expected comments");
        };
        assert!(resolved.is_empty());
    }

    #[test]
    fn agent_reply_flips_status_persists_and_toasts() {
        let (fixture, mut app, id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::ReplyComment {
            id: id.clone(),
            body: "it is the answer".to_owned(),
        });
        assert_eq!(
            response,
            McpResponse::Replied {
                status: "replied".to_owned()
            }
        );
        let comment = &app.review.session.comments[0];
        assert_eq!(comment.status, CommentStatus::Replied);
        assert_eq!(comment.replies[0].author, AGENT_AUTHOR);
        let message = app.message.clone().expect("toast");
        assert!(message.text.contains("agent replied"));
        // the agent's own mutation must not wake its feedback poll
        assert_eq!(app.feedback_epoch(), 0);
        let reloaded = diffler_core::store::load(&fixture.root).unwrap();
        assert_eq!(reloaded.comments[0].status, CommentStatus::Replied);
    }

    #[test]
    fn agent_reply_to_unknown_id_errors() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::ReplyComment {
            id: "nope".to_owned(),
            body: "hello".to_owned(),
        });
        assert!(matches!(response, McpResponse::Error(message) if message.contains("nope")));
    }

    #[test]
    fn propose_resolve_on_an_empty_thread_leaves_the_note_and_stays_replied() {
        let (_fixture, mut app, id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::ProposeResolve {
            id,
            note: Some("fixed in abc123".to_owned()),
        });
        assert_eq!(
            response,
            McpResponse::Replied {
                status: "replied".to_owned()
            }
        );
        let comment = &app.review.session.comments[0];
        assert_eq!(comment.status, CommentStatus::Replied, "not resolved");
        assert_eq!(
            comment
                .replies
                .iter()
                .map(|r| r.body.as_str())
                .collect::<Vec<_>>(),
            vec!["fixed in abc123"],
            "a bare flag still leaves its reasoning"
        );
    }

    /// The agent answers, then flags. The flag is a status change, so the card
    /// carries one reply rather than the answer plus a summary of it.
    #[test]
    fn propose_resolve_after_a_reply_writes_nothing() {
        let (_fixture, mut app, id) = app_with_comment();
        app.handle_mcp(McpRequestKind::ReplyComment {
            id: id.clone(),
            body: "checked the job: the host is right".to_owned(),
        });

        app.handle_mcp(McpRequestKind::ProposeResolve {
            id: id.clone(),
            note: Some("host and idProperty confirmed".to_owned()),
        });
        app.handle_mcp(McpRequestKind::ProposeResolve { id, note: None });

        let comment = &app.review.session.comments[0];
        assert_eq!(
            comment
                .replies
                .iter()
                .map(|r| r.body.as_str())
                .collect::<Vec<_>>(),
            vec!["checked the job: the host is right"],
            "the answer is the only thing in the thread"
        );
        assert_eq!(comment.status, CommentStatus::Replied);
    }

    #[test]
    fn propose_resolve_unknown_id_errors_without_touching_the_session() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::ProposeResolve {
            id: "nope".to_owned(),
            note: Some("done".to_owned()),
        });
        assert!(matches!(response, McpResponse::Error(message) if message.contains("nope")));
        let comment = &app.review.session.comments[0];
        assert_eq!(comment.status, CommentStatus::Open);
        assert!(comment.replies.is_empty());
    }

    #[test]
    fn mark_viewed_reflects_in_review_status() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::MarkViewed {
            file: "src/lib.rs".to_owned(),
        });
        assert_eq!(response, McpResponse::Ok);
        assert!(app.is_path_viewed("src/lib.rs"));

        let response = app.handle_mcp(McpRequestKind::MarkViewed {
            file: "nope.rs".to_owned(),
        });
        assert!(matches!(response, McpResponse::Error(_)));
    }

    fn commit_anchor(file: &str) -> Anchor {
        Anchor {
            file: file.to_owned(),
            line: Some(1),
            line_end: None,
            on_old_side: false,
            line_text: None,
        }
    }

    #[test]
    fn get_comments_aggregates_every_source_and_tags_provenance() {
        let (_fixture, mut app, working_id) = app_with_comment();
        let oid = app.status.recent[0].oid.clone();
        let source = ReviewSource::commit(&oid);
        app.review.ensure_source(&source).expect("ensure");
        let commit_id = app
            .review
            .session_for_mut(&source)
            .add_comment(commit_anchor("src/lib.rs"), "human", "on the commit")
            .id
            .clone();
        app.review.save_for(&source).expect("save");

        let McpResponse::Comments(all) =
            app.handle_mcp(McpRequestKind::GetComments { status: None })
        else {
            panic!("expected comments");
        };
        let working = all.iter().find(|c| c.id == working_id).expect("working");
        assert_eq!(working.source, "working");
        assert_eq!(working.source_label, "working tree");
        let on_commit = all.iter().find(|c| c.id == commit_id).expect("commit");
        assert_eq!(on_commit.source, source.key());
        assert_eq!(on_commit.source_label, source.label());
    }

    #[test]
    fn agent_reply_targets_the_comment_owning_source() {
        let (fixture, mut app, _working_id) = app_with_comment();
        let oid = app.status.recent[0].oid.clone();
        let source = ReviewSource::commit(&oid);
        app.review.ensure_source(&source).expect("ensure");
        let id = app
            .review
            .session_for_mut(&source)
            .add_comment(commit_anchor("src/lib.rs"), "human", "why here?")
            .id
            .clone();
        app.review.save_for(&source).expect("save");

        let response = app.handle_mcp(McpRequestKind::ReplyComment {
            id,
            body: "because".to_owned(),
        });
        assert_eq!(
            response,
            McpResponse::Replied {
                status: "replied".to_owned()
            }
        );
        // the reply persists under the commit source, not the working tree
        let reloaded = diffler_core::store::load_source(&fixture.root, &source).expect("load");
        assert_eq!(reloaded.comments[0].status, CommentStatus::Replied);
        assert_eq!(reloaded.comments[0].replies[0].author, AGENT_AUTHOR);
        assert!(
            app.review
                .session
                .comments
                .iter()
                .all(|c| c.replies.is_empty()),
            "the working-tree comment is untouched"
        );
    }

    #[test]
    fn list_reviews_enumerates_sources_with_counts() {
        let (_fixture, mut app, _id) = app_with_comment();
        let oid = app.status.recent[0].oid.clone();
        let source = ReviewSource::commit(&oid);
        app.review.ensure_source(&source).expect("ensure");
        app.review
            .session_for_mut(&source)
            .add_comment(commit_anchor("src/lib.rs"), "human", "x");
        app.review.save_for(&source).expect("save");
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };

        let McpResponse::Reviews(reviews) = app.handle_mcp(McpRequestKind::ListReviews) else {
            panic!("expected reviews");
        };
        let working = reviews
            .iter()
            .find(|r| r.source == "working")
            .expect("working review");
        assert_eq!(working.open_comments, 1);
        let commit = reviews
            .iter()
            .find(|r| r.source == source.key())
            .expect("commit review");
        assert_eq!(commit.open_comments, 1);
        assert_eq!(commit.label, source.label());
        let walkthrough_source = ReviewSource::walkthrough(&published.id);
        let walkthrough = reviews
            .iter()
            .find(|r| r.source == walkthrough_source.key())
            .expect("walkthrough review, a file in reviews/ like any other");
        assert_eq!(walkthrough.open_comments, 1, "the stop is its own comment");
        assert_eq!(walkthrough.label, "tour", "the walkthrough's own title");
    }

    #[test]
    fn agent_mark_viewed_targets_the_open_review_source() {
        let (_fixture, mut app, _id) = app_with_comment();
        let oid = app.status.recent[0].oid.clone();
        app.open_commit_diff(&oid);

        let response = app.handle_mcp(McpRequestKind::MarkViewed {
            file: "src/lib.rs".to_owned(),
        });
        assert_eq!(response, McpResponse::Ok);

        let source = ReviewSource::commit(&oid);
        assert!(
            app.review
                .session_for(&source)
                .viewed
                .contains_key("src/lib.rs"),
            "viewed lands on the open commit review"
        );
        assert!(
            app.review.session.viewed.is_empty(),
            "the working-tree review is untouched"
        );
    }

    #[test]
    fn feedback_returns_open_and_replied_but_not_resolved() {
        let (_fixture, mut app, id) = app_with_comment();
        app.review.session.add_comment(
            Anchor {
                file: "todo.md".to_owned(),
                line: None,
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "human",
            "second",
        );
        app.review.session.resolve(&id);
        let McpResponse::Feedback { comments, .. } = app.handle_mcp(McpRequestKind::Feedback)
        else {
            panic!("expected comments");
        };
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "second");
    }

    fn stop(title: &str, anchor: Option<&str>, body: &str) -> StopParams {
        StopParams {
            id: None,
            title: title.to_owned(),
            anchor: anchor.map(str::to_owned),
            body: body.to_owned(),
            notes: None,
        }
    }

    fn note(anchor: Option<&str>, body: &str) -> NoteParams {
        NoteParams {
            id: None,
            anchor: anchor.map(str::to_owned),
            body: body.to_owned(),
        }
    }

    fn stop_with_notes(
        title: &str,
        anchor: Option<&str>,
        body: &str,
        notes: Vec<NoteParams>,
    ) -> StopParams {
        let mut stop = stop(title, anchor, body);
        stop.notes = Some(notes);
        stop
    }

    /// The session of the walkthrough source `id` names, once ensured.
    fn walkthrough_session<'a>(app: &'a mut App, id: &str) -> &'a diffler_core::session::Session {
        let source = ReviewSource::walkthrough(id);
        app.review.ensure_source(&source).expect("ensure source");
        app.review.session_for(&source)
    }

    /// The comment ids the walkthrough `id` is made of, in reading order.
    fn stop_ids(app: &mut App, id: &str) -> Vec<String> {
        walkthrough_session(app, id)
            .walkthrough
            .as_ref()
            .map_or_else(Vec::new, |walkthrough| walkthrough.stops.clone())
    }

    #[test]
    fn publishing_three_stops_stores_them_and_review_status_reports_the_count() {
        let (_fixture, mut app, _id) = app_with_comment();
        let stops = (0..3)
            .map(|i| stop(&format!("stop {i}"), None, "why"))
            .collect();
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops,
            skipped: None,
            summary: None,
        });
        let McpResponse::WalkthroughPublished(published) = response else {
            panic!("expected a published walkthrough: {response:?}");
        };
        assert_eq!(published.stops, 3);
        assert!(published.receipts.is_empty(), "{:?}", published.receipts);

        let McpResponse::Status(status) = app.handle_mcp(McpRequestKind::ReviewStatus) else {
            panic!("expected a status response");
        };
        let walkthrough = status.walkthroughs.first().expect("a walkthrough");
        assert_eq!(walkthrough.stops, 3);
        assert_eq!(walkthrough.title, "tour");
    }

    /// A stop is an agent comment, which is what puts it in the comments pane
    /// and gives the human a thread to answer it in.
    #[test]
    fn publishing_makes_one_agent_comment_per_stop() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![
                    stop("The answer", Some("src/lib.rs#answer"), "why 42"),
                    stop("What is left", None, "an overview"),
                ],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let ids = stop_ids(&mut app, &published.id);
        assert_eq!(ids.len(), 2);
        let session = walkthrough_session(&mut app, &published.id);
        let stops: Vec<_> = ids
            .iter()
            .map(|id| session.comment(id).expect("a stop comment"))
            .collect();
        assert_eq!(stops[0].author, AGENT_AUTHOR);
        assert_eq!(stops[0].title.as_deref(), Some("The answer"));
        assert_eq!(stops[0].anchor_ref.as_deref(), Some("src/lib.rs#answer"));
        assert_eq!(stops[0].anchor.file, "src/lib.rs");
        assert_eq!(stops[0].anchor.line, None, "the worker fills the lines");
        assert_eq!(stops[1].anchor_ref, None);
        assert_eq!(
            stops[1].anchor.file, "src/lib.rs",
            "an anchorless stop hangs on the first file the walkthrough names"
        );
    }

    /// Publishing twice with no `id` creates two separate walkthrough
    /// sources, side by side, rather than replacing the first.
    #[test]
    fn publishing_twice_without_an_id_gives_two_walkthroughs() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(first) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "first tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let McpResponse::WalkthroughPublished(second) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "second tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        assert_ne!(first.id, second.id);
        assert!(
            walkthrough_session(&mut app, &first.id)
                .walkthrough
                .is_some()
        );
        assert!(
            walkthrough_session(&mut app, &second.id)
                .walkthrough
                .is_some()
        );
    }

    /// An id an agent supplies is arbitrary text, not a filesystem-safe oid: a
    /// `/` in it must not turn the review key into a path whose directory
    /// does not exist. The walkthrough still saves, and a fresh read of the
    /// same id finds it.
    #[test]
    fn a_walkthrough_id_with_a_slash_still_saves_and_loads() {
        let (fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: Some("feature/login".to_owned()),
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        assert_eq!(published.id, "feature/login");

        let source = ReviewSource::walkthrough("feature/login");
        let reloaded = diffler_core::store::load_source(&fixture.root, &source).expect("load");
        assert!(
            reloaded.walkthrough.is_some(),
            "the walkthrough landed on disk"
        );

        let McpResponse::Walkthrough(Some(info)) = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some("feature/login".to_owned()),
        }) else {
            panic!("expected the walkthrough back by its own id");
        };
        assert_eq!(info.title, "tour");
    }

    /// A publish that cannot write its review file must not claim success:
    /// the agent is told the save failed rather than being handed an id for
    /// a walkthrough that was never persisted.
    #[cfg(unix)]
    #[test]
    fn a_walkthrough_that_fails_to_save_is_reported_not_claimed_published() {
        use std::os::unix::fs::PermissionsExt;

        let (fixture, mut app, _id) = app_with_comment();
        let reviews_dir = fixture.root.join(".diffler/reviews");
        std::fs::create_dir_all(&reviews_dir).expect("create reviews dir");
        std::fs::set_permissions(&reviews_dir, std::fs::Permissions::from_mode(0o555))
            .expect("make the reviews dir read-only");

        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: Some("boom".to_owned()),
            title: "tour".to_owned(),
            stops: vec![stop("one", None, "why")],
            skipped: None,
            summary: None,
        });

        std::fs::set_permissions(&reviews_dir, std::fs::Permissions::from_mode(0o755))
            .expect("restore permissions for cleanup");

        assert!(
            matches!(response, McpResponse::Error(_)),
            "a save that fails must not be reported as published: {response:?}"
        );
    }

    /// Revising is how an agent answers feedback: passing the walkthrough's
    /// own `id` back revises that exact source, leaving any other walkthrough
    /// untouched; a stop that passes its own id back keeps its thread, and
    /// the stops it drops go with the old revision.
    #[test]
    fn republishing_with_the_walkthroughs_id_revises_it_and_leaves_the_other_one() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(first) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("first", None, "why"), stop("second", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let ids = stop_ids(&mut app, &first.id);
        let kept = ids[0].clone();
        let dropped = ids[1].clone();
        {
            let source = ReviewSource::walkthrough(&first.id);
            app.review
                .session_for_mut(&source)
                .reply(&kept, "reviewer", "say more");
        }

        let McpResponse::WalkthroughPublished(other) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "an unrelated tour".to_owned(),
                stops: vec![stop("elsewhere", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };

        let mut revised = stop("first, revised", Some("src/lib.rs#answer"), "a better why");
        revised.id = Some(kept.clone());
        app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: Some(first.id.clone()),
            title: "tour".to_owned(),
            stops: vec![revised, stop("third", None, "why")],
            skipped: None,
            summary: None,
        });

        let session = walkthrough_session(&mut app, &first.id);
        let comment = session.comment(&kept).expect("the kept stop");
        assert_eq!(comment.replies.len(), 1, "the thread survived");
        assert_eq!(comment.title.as_deref(), Some("first, revised"));
        assert_eq!(comment.body, "a better why");
        assert_eq!(comment.anchor_ref.as_deref(), Some("src/lib.rs#answer"));
        assert!(
            session.comment(&dropped).is_none(),
            "a stop nobody passed back is gone"
        );
        assert_eq!(
            session.walkthrough.as_ref().and_then(|w| w.stops.first()),
            Some(&kept)
        );
        assert!(
            walkthrough_session(&mut app, &other.id)
                .walkthrough
                .is_some(),
            "the unrelated walkthrough is untouched, still there"
        );
    }

    #[test]
    fn publishing_one_stop_over_the_rail_is_refused() {
        let (_fixture, mut app, _id) = app_with_comment();
        let stops = (0..=diffler_core::walkthrough::MAX_STOPS)
            .map(|i| stop(&format!("s{i}"), None, "why"))
            .collect();
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "too many".to_owned(),
            stops,
            skipped: None,
            summary: None,
        });
        let McpResponse::Error(text) = response else {
            panic!("expected a refusal: {response:?}");
        };
        assert!(text.contains("too_many_stops"), "{text}");
        assert!(
            app.review
                .all_reviews()
                .expect("all reviews")
                .into_iter()
                .all(
                    |(s, session)| !matches!(s, ReviewSource::Walkthrough { .. })
                        || session.walkthrough.is_none()
                ),
            "a refused walkthrough is never stored"
        );
    }

    #[test]
    fn publishing_with_a_summary_stores_it_and_get_walkthrough_returns_it() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: Some("the shape of the change".to_owned()),
            })
        else {
            panic!("expected a published walkthrough");
        };

        let McpResponse::Walkthrough(Some(info)) = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some(published.id),
        }) else {
            panic!("expected a walkthrough");
        };
        assert_eq!(info.summary.as_deref(), Some("the shape of the change"));
    }

    #[test]
    fn publishing_with_no_summary_leaves_it_none() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };

        let McpResponse::Walkthrough(Some(info)) = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some(published.id),
        }) else {
            panic!("expected a walkthrough");
        };
        assert_eq!(info.summary, None);
    }

    /// Publishing stamps the walkthrough with the full oid of whatever is
    /// checked out right now, so its anchors resolve against the code they
    /// actually describe even after the branch moves on. Both the publish
    /// response and a later `get_walkthrough` report it.
    #[test]
    fn publishing_stamps_the_current_head() {
        let (_fixture, mut app, _id) = app_with_comment();
        let head = app.review.vcs.resolve("HEAD").expect("resolve head");
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        assert_eq!(published.rev, Some(head.clone()));

        let McpResponse::Walkthrough(Some(info)) = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some(published.id),
        }) else {
            panic!("expected a walkthrough");
        };
        assert_eq!(info.rev, Some(head));
    }

    /// A revision restamps to whatever HEAD is now: it redescribes the stops
    /// against the code currently checked out, not the code an earlier
    /// revision of this same walkthrough was pinned to.
    #[test]
    fn republishing_restamps_to_the_new_head() {
        let (fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(first) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };

        fixture.write("todo.md", "- [ ] more\n");
        fixture.commit_all("move head on");
        let after = app.review.vcs.resolve("HEAD").expect("resolve head");
        assert_ne!(first.rev, Some(after.clone()), "the fixture moved HEAD");

        let McpResponse::WalkthroughPublished(second) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: Some(first.id.clone()),
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "still why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a revised walkthrough");
        };
        assert_eq!(second.rev, Some(after));
    }

    /// A revision that passes the same summary back keeps it, the way a stop
    /// passing its own id back keeps its thread.
    #[test]
    fn a_revision_that_passes_the_same_summary_back_keeps_it() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: Some("the shape of the change".to_owned()),
            })
        else {
            panic!("expected a published walkthrough");
        };

        app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: Some(published.id.clone()),
            title: "tour, revised".to_owned(),
            stops: vec![stop("one", None, "why")],
            skipped: None,
            summary: Some("the shape of the change".to_owned()),
        });

        let McpResponse::Walkthrough(Some(info)) = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some(published.id),
        }) else {
            panic!("expected a walkthrough");
        };
        assert_eq!(info.summary.as_deref(), Some("the shape of the change"));
    }

    #[test]
    fn an_oversized_summary_is_refused_with_the_body_cap_receipt() {
        let (_fixture, mut app, _id) = app_with_comment();
        let oversized = "x".repeat(BODY_MAX_BYTES + 1);
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![stop("one", None, "why")],
            skipped: None,
            summary: Some(oversized),
        });
        let McpResponse::Error(text) = response else {
            panic!("expected a refusal: {response:?}");
        };
        assert!(text.contains("body_too_long"), "{text}");
        assert!(
            app.review
                .all_reviews()
                .expect("all reviews")
                .into_iter()
                .all(
                    |(s, session)| !matches!(s, ReviewSource::Walkthrough { .. })
                        || session.walkthrough.is_none()
                ),
            "a refused walkthrough is never stored"
        );
    }

    #[test]
    fn a_bare_path_anchor_yields_an_anchor_whole_receipt() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![stop("the file", Some("src/lib.rs"), "why")],
            skipped: None,
            summary: None,
        });
        let McpResponse::WalkthroughPublished(published) = response else {
            panic!("expected a published walkthrough: {response:?}");
        };
        assert!(
            published
                .receipts
                .iter()
                .any(|r| r.code == "anchor_whole" && r.stop == Some(0)),
            "{:?}",
            published.receipts
        );
    }

    #[test]
    fn a_sequence_diagram_fence_yields_a_figure_dropped_receipt() {
        let (_fixture, mut app, _id) = app_with_comment();
        let body = "```mermaid\nsequenceDiagram\n  a->>b: hi\n```\n";
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![stop("a figure", None, body)],
            skipped: None,
            summary: None,
        });
        let McpResponse::WalkthroughPublished(published) = response else {
            panic!("expected a published walkthrough: {response:?}");
        };
        assert!(
            published
                .receipts
                .iter()
                .any(|r| r.code == "figure_dropped"),
            "{:?}",
            published.receipts
        );
    }

    #[test]
    fn get_walkthrough_round_trips() {
        let (_fixture, mut app, _id) = app_with_comment();
        assert_eq!(
            app.handle_mcp(McpRequestKind::GetWalkthrough { id: None }),
            McpResponse::Walkthrough(None)
        );
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", Some("src/lib.rs#answer"), "why")],
                skipped: Some("left out the rest".to_owned()),
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let McpResponse::Walkthrough(Some(info)) =
            app.handle_mcp(McpRequestKind::GetWalkthrough { id: None })
        else {
            panic!("expected a walkthrough");
        };
        assert_eq!(info.title, "tour");
        assert_eq!(info.skipped.as_deref(), Some("left out the rest"));
        assert_eq!(info.stops.len(), 1);
        assert_eq!(info.stops[0].title, "one");
        assert_eq!(info.stops[0].anchor.as_deref(), Some("src/lib.rs#answer"));
        assert_eq!(
            Some(&info.stops[0].id),
            stop_ids(&mut app, &published.id).first(),
            "the comment id, which is what a revision passes back"
        );
    }

    /// An id nobody has published under is a real "no such walkthrough",
    /// distinct from a read that failed.
    #[test]
    fn get_walkthrough_by_an_unknown_id_returns_none_not_an_error() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some("nope".to_owned()),
        });
        assert_eq!(response, McpResponse::Walkthrough(None));
    }

    /// A review file that fails to parse is a real error, not the same
    /// "nothing here" a genuinely unpublished id answers with: an id
    /// `review_status` just listed must never come back silently null.
    #[test]
    fn get_walkthrough_surfaces_a_read_failure_instead_of_none() {
        let (fixture, mut app, _id) = app_with_comment();
        std::fs::create_dir_all(fixture.root.join(".diffler/reviews")).expect("mkdir");
        std::fs::write(
            fixture
                .root
                .join(".diffler/reviews/walkthrough-broken.json"),
            "{not json",
        )
        .expect("write corrupt file");

        let response = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some("broken".to_owned()),
        });
        assert!(
            matches!(response, McpResponse::Error(_)),
            "a corrupt file must not read back as no walkthrough: {response:?}"
        );
    }

    /// `get_walkthrough` fetches a specific walkthrough by id, or the newest
    /// one when `id` is omitted.
    #[test]
    fn get_walkthrough_by_id_and_by_default() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(first) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "first tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let McpResponse::WalkthroughPublished(second) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "second tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        // both publishes can land in the same wall-clock second; force them
        // apart so "the newest one" has something real to pick
        if let Some(w) = app
            .review
            .session_for_mut(&ReviewSource::walkthrough(&first.id))
            .walkthrough
            .as_mut()
        {
            w.at = 1;
        }
        if let Some(w) = app
            .review
            .session_for_mut(&ReviewSource::walkthrough(&second.id))
            .walkthrough
            .as_mut()
        {
            w.at = 2;
        }

        let McpResponse::Walkthrough(Some(info)) = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some(first.id.clone()),
        }) else {
            panic!("expected the first walkthrough");
        };
        assert_eq!(info.title, "first tour");

        let McpResponse::Walkthrough(Some(info)) =
            app.handle_mcp(McpRequestKind::GetWalkthrough { id: None })
        else {
            panic!("expected the newest walkthrough");
        };
        assert_eq!(info.title, "second tour", "the default is the newest one");
        assert_eq!(info.id, second.id);
    }

    /// `review_status` lists every walkthrough of the review, newest first.
    #[test]
    fn review_status_lists_every_walkthrough_newest_first() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(first) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "first tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let McpResponse::WalkthroughPublished(second) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "second tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        // both publishes can land in the same wall-clock second; force them
        // apart so the newest-first sort has something real to sort on
        if let Some(w) = app
            .review
            .session_for_mut(&ReviewSource::walkthrough(&first.id))
            .walkthrough
            .as_mut()
        {
            w.at = 1;
        }
        if let Some(w) = app
            .review
            .session_for_mut(&ReviewSource::walkthrough(&second.id))
            .walkthrough
            .as_mut()
        {
            w.at = 2;
        }

        let McpResponse::Status(status) = app.handle_mcp(McpRequestKind::ReviewStatus) else {
            panic!("expected a status response");
        };
        let titles: Vec<&str> = status
            .walkthroughs
            .iter()
            .map(|w| w.title.as_str())
            .collect();
        assert_eq!(titles, vec!["second tour", "first tour"]);
    }

    /// A corrupt review file must not hide every other review or walkthrough
    /// behind a false "clean repository": `review_status` names the file
    /// instead, and `list_reviews` keeps listing everything that did parse.
    #[test]
    fn a_corrupt_review_file_is_named_not_hidden_and_never_hides_the_rest() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let reviews_dir = app.review.repo_root.join(".diffler/reviews");
        std::fs::write(reviews_dir.join("walkthrough-broken.json"), "{not json")
            .expect("write corrupt file");

        let McpResponse::Status(status) = app.handle_mcp(McpRequestKind::ReviewStatus) else {
            panic!("expected a status response");
        };
        assert_eq!(status.corrupt_reviews, vec!["walkthrough-broken.json"]);
        assert!(
            status.walkthroughs.iter().any(|w| w.id == published.id),
            "the good walkthrough is still listed"
        );

        let McpResponse::Reviews(reviews) = app.handle_mcp(McpRequestKind::ListReviews) else {
            panic!("expected reviews");
        };
        assert!(
            reviews.iter().any(|r| r.source == "working"),
            "list_reviews is not disrupted by the corrupt file either"
        );
    }

    /// The status screen reads the session's walkthrough at draw time, so a
    /// publish over MCP has to show up on the very next render, with no
    /// keypress in between.
    #[test]
    fn publishing_over_mcp_shows_on_the_status_screen_without_a_keypress() {
        let (_fixture, mut app, _id) = app_with_comment();
        let (reply, mut rx) = tokio::sync::oneshot::channel();
        let flow = app.handle(crate::event::AppEvent::Mcp(crate::mcp::McpRequest {
            kind: McpRequestKind::PublishWalkthrough {
                id: None,
                title: "the tour".to_owned(),
                stops: vec![stop("one", None, "why")],
                skipped: None,
                summary: None,
            },
            reply,
        }));
        assert_eq!(flow, crate::app::Flow::Continue);
        assert!(matches!(
            rx.try_recv(),
            Ok(McpResponse::WalkthroughPublished(_))
        ));

        let content = crate::test_support::render(&mut app).backend().to_string();
        assert!(content.contains("Walkthrough"), "{content}");
    }

    /// A comment on a stop is a reply on that stop's own comment, so the id in
    /// the feedback is how the agent knows which stop the human is talking
    /// about.
    #[test]
    fn feedback_carries_a_reply_on_the_stop_it_answers() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("one", None, "why"), stop("two", None, "why")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let second = stop_ids(&mut app, &published.id)[1].clone();
        let source = ReviewSource::walkthrough(&published.id);
        app.review
            .session_for_mut(&source)
            .reply(&second, "reviewer", "why not 43?");

        let McpResponse::Feedback { comments } = app.handle_mcp(McpRequestKind::Feedback) else {
            panic!("expected feedback");
        };
        let answered = comments
            .iter()
            .find(|comment| comment.id == second)
            .expect("the stop the human replied on");
        assert_eq!(answered.replies.len(), 1);
        assert_eq!(answered.replies[0].body, "why not 43?");
    }

    /// A stop's `notes` are its own remarks on other parts of the same
    /// region: each becomes an agent comment beside the stop's, and the
    /// walkthrough owns all of them so a republish can tell them from a
    /// human's.
    #[test]
    fn publishing_a_stops_notes_makes_extra_agent_comments() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop_with_notes(
                    "The answer",
                    Some("src/lib.rs#answer"),
                    "why 42",
                    vec![
                        note(None, "a first remark"),
                        note(Some("src/lib.rs:2"), "a second remark"),
                    ],
                )],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };

        let session = walkthrough_session(&mut app, &published.id);
        let walkthrough = session.walkthrough.clone().expect("a walkthrough");
        assert_eq!(walkthrough.stops.len(), 1);
        assert_eq!(session.comments.len(), 3, "{:?}", session.comments);
        let notes: Vec<_> = session
            .comments
            .iter()
            .filter(|c| !walkthrough.stops.contains(&c.id))
            .collect();
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|note| note.author == AGENT_AUTHOR));
        assert!(notes.iter().all(|note| note.title.is_none()));

        let McpResponse::Walkthrough(Some(info)) = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some(published.id),
        }) else {
            panic!("expected a walkthrough");
        };
        assert_eq!(info.stops[0].notes.len(), 2);
    }

    /// A note anchored outside its stop's own file would show up in another
    /// slide entirely, so it is refused rather than silently misplaced.
    #[test]
    fn a_note_naming_another_file_than_its_stop_is_refused() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![stop_with_notes(
                "The answer",
                Some("src/lib.rs#answer"),
                "why 42",
                vec![note(Some("ci.yml:1"), "wrong file")],
            )],
            skipped: None,
            summary: None,
        });
        let McpResponse::Error(text) = response else {
            panic!("expected a refusal: {response:?}");
        };
        assert!(text.contains("note_outside_stop"), "{text}");
        assert!(
            app.review
                .all_reviews()
                .expect("all reviews")
                .into_iter()
                .all(
                    |(s, session)| !matches!(s, ReviewSource::Walkthrough { .. })
                        || session.walkthrough.is_none()
                ),
            "a refused walkthrough is never stored"
        );
    }

    /// Two stops passing back the same id would have the second write find
    /// the comment the first just made and overwrite it, silently losing the
    /// first stop. Refused instead, naming the repeated id.
    #[test]
    fn two_stops_sharing_an_id_are_refused() {
        let (_fixture, mut app, _id) = app_with_comment();
        let mut first = stop("first", None, "why");
        first.id = Some("shared".to_owned());
        let mut second = stop("second", None, "why");
        second.id = Some("shared".to_owned());
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![first, second],
            skipped: None,
            summary: None,
        });
        let McpResponse::Error(text) = response else {
            panic!("expected a refusal: {response:?}");
        };
        assert!(text.contains("duplicate_id"), "{text}");
        assert!(text.contains("shared"), "{text}");
        assert!(
            app.review
                .all_reviews()
                .expect("all reviews")
                .into_iter()
                .all(
                    |(s, session)| !matches!(s, ReviewSource::Walkthrough { .. })
                        || session.walkthrough.is_none()
                ),
            "a refused walkthrough is never stored"
        );
    }

    /// The same collision applies between a stop and one of its own notes:
    /// nothing distinguishes their ids from each other in the write loop.
    #[test]
    fn a_stop_and_its_own_note_sharing_an_id_are_refused() {
        let (_fixture, mut app, _id) = app_with_comment();
        let mut collides = note(None, "a remark");
        collides.id = Some("shared".to_owned());
        let mut stop = stop_with_notes("the stop", None, "why", vec![collides]);
        stop.id = Some("shared".to_owned());
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![stop],
            skipped: None,
            summary: None,
        });
        let McpResponse::Error(text) = response else {
            panic!("expected a refusal: {response:?}");
        };
        assert!(text.contains("duplicate_id"), "{text}");
    }

    /// A note's thread survives a revision the same way a stop's does: pass
    /// its id back to keep it, leave it out and it goes with the rest of the
    /// old walkthrough.
    #[test]
    fn republishing_keeps_a_note_passed_back_by_id_and_deletes_the_rest() {
        let (_fixture, mut app, _id) = app_with_comment();
        app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![stop_with_notes(
                "The answer",
                Some("src/lib.rs#answer"),
                "why 42",
                vec![note(None, "first"), note(None, "second")],
            )],
            skipped: None,
            summary: None,
        });
        let McpResponse::Walkthrough(Some(info)) =
            app.handle_mcp(McpRequestKind::GetWalkthrough { id: None })
        else {
            panic!("expected a walkthrough");
        };
        let walkthrough_id = info.id.clone();
        let stop_id = info.stops[0].id.clone();
        let kept_note = info.stops[0].notes[0].id.clone();
        let dropped_note = info.stops[0].notes[1].id.clone();
        let source = ReviewSource::walkthrough(&walkthrough_id);
        app.review
            .session_for_mut(&source)
            .reply(&kept_note, "reviewer", "say more");

        let mut kept = note(None, "first, revised");
        kept.id = Some(kept_note.clone());
        let mut revised = stop_with_notes(
            "The answer",
            Some("src/lib.rs#answer"),
            "why 42, revised",
            vec![kept],
        );
        revised.id = Some(stop_id);
        app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: Some(walkthrough_id.clone()),
            title: "tour".to_owned(),
            stops: vec![revised],
            skipped: None,
            summary: None,
        });

        let session = app.review.session_for(&source);
        let kept = session.comment(&kept_note).expect("the kept note");
        assert_eq!(kept.replies.len(), 1, "the thread survived");
        assert_eq!(kept.body, "first, revised");
        assert!(
            session.comment(&dropped_note).is_none(),
            "a note nobody passed back is gone"
        );
    }
}
