//! App-side handling of agent tool calls. Runs synchronously on the main
//! loop against the owned review state; the `mcp` module only ships
//! requests here and renders the responses.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use diffler_core::model::DiffModel;
use diffler_core::session::{Anchor, Comment, CommentStatus, now_unix};
use diffler_core::source::ReviewSource;
use diffler_core::vcs::VcsError;
use diffler_core::walkthrough::{
    BODY_MAX_BYTES, MAX_STOPS, Receipt, ReceiptCode, TOTAL_MAX_BYTES, Target, Walkthrough,
};

use super::App;
use crate::mcp::{
    AGENT_AUTHOR, CommentInfo, FileEntry, McpRequestKind, McpResponse, NoteInfo, NoteParams,
    ProjectInfo, ReceiptInfo, ReviewStatusResponse, ReviewSummary, StopInfo, StopParams,
    WalkthroughInfo, WalkthroughPublished, WalkthroughSummary, comment_info, comment_status_name,
    file_status_name, render_unified,
};

impl App {
    pub(crate) fn handle_mcp(&mut self, kind: McpRequestKind) -> McpResponse {
        self.record_mcp_activity(&kind);
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
            McpRequestKind::AddComment {
                file,
                line,
                line_end,
                body,
                as_human,
            } => self.agent_add_comment(&file, line, line_end, &body, as_human),
            McpRequestKind::DeleteComment { id } => self.agent_delete_comment(&id),
            McpRequestKind::EditComment { id, body } => self.agent_edit_comment(&id, &body),
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
            McpRequestKind::ReportActivity { .. } => McpResponse::Ok,
            McpRequestKind::OpenProject { .. } => {
                McpResponse::Error("only the workspace holding the tabs opens a project".to_owned())
            }
        }
    }

    /// We count every tool call as activity, so an agent that never calls
    /// `report_activity` still shows as busy.
    pub(crate) fn record_mcp_activity(&mut self, kind: &McpRequestKind) {
        let (focus, file): (&str, Option<&str>) = match kind {
            McpRequestKind::ReportActivity { focus, file } => (focus, file.as_deref()),
            McpRequestKind::ReviewStatus => ("checking the review", None),
            McpRequestKind::GetDiff { file } => ("reading the diff", file.as_deref()),
            McpRequestKind::GetComments { .. } => ("reading comments", None),
            McpRequestKind::ListReviews => ("listing reviews", None),
            McpRequestKind::ReplyComment { .. } => ("replying to a comment", None),
            McpRequestKind::ProposeResolve { .. } => ("flagging a comment addressed", None),
            McpRequestKind::MarkViewed { file } => ("marking a file viewed", Some(file.as_str())),
            McpRequestKind::AddComment { file, .. } => ("writing a comment", Some(file.as_str())),
            McpRequestKind::DeleteComment { .. } => ("deleting a comment", None),
            McpRequestKind::EditComment { .. } => ("editing a comment", None),
            McpRequestKind::Feedback => ("reading feedback", None),
            McpRequestKind::PublishWalkthrough { .. } => ("publishing a walkthrough", None),
            McpRequestKind::GetWalkthrough { .. } => ("reading the walkthrough", None),
            McpRequestKind::OpenProject { .. } => ("opening a project", None),
        };
        self.set_agent_activity(focus, file);
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
        // we name unparsable review files so the agent knows the lists below may be incomplete
        let corrupt_reviews = self
            .review
            .all_reviews_and_corrupt()
            .map_or_else(|_| Vec::new(), |(_, corrupt)| corrupt)
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        ReviewStatusResponse {
            repo: self.project_name(),
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
            projects: Vec::new(),
        }
    }

    pub(crate) fn project_name(&self) -> String {
        self.review
            .repo_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub(crate) fn project_info(&self, active: bool) -> ProjectInfo {
        let (open, replied) =
            self.review_summaries()
                .iter()
                .fold((0, 0), |(open, replied), review| {
                    (
                        open + review.open_comments,
                        replied + review.replied_comments,
                    )
                });
        ProjectInfo {
            name: self.project_name(),
            root: self.review.repo_root.display().to_string(),
            active,
            open_comments: open,
            replied_comments: replied,
        }
    }

    pub(crate) fn owns_id(&self, id: &str) -> bool {
        self.review.all_reviews().is_ok_and(|reviews| {
            reviews.iter().any(|(source, session)| {
                matches!(source, ReviewSource::Walkthrough { id: own } if own == id)
                    || session.comments.iter().any(|comment| comment.id == id)
            })
        })
    }

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

    /// The newest walkthrough when `id` is `None`. `Ok(None)` means none
    /// exists; a corrupt review file is `Err`.
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

    /// Creates a walkthrough source, or revises the one `id` names. A stop
    /// passing its comment id back keeps that thread; every other agent
    /// comment in the source is dropped. Unresolvable anchors refuse the
    /// publish; figure receipts are only reported.
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
        // a revision redescribes the stops against the current checkout, so we repin it to HEAD
        let rev = self.review.vcs.resolve("HEAD").ok();
        // a walkthrough's own diff names no review, so revising from it keeps the old `about`
        let about = match self.active_review_source() {
            ReviewSource::Walkthrough { .. } => self
                .review
                .session_for(&source)
                .walkthrough
                .as_ref()
                .map_or(ReviewSource::WorkingTree, |w| w.about.clone()),
            other => other,
        };
        let model = self.source_model(&about);
        let files = match self.resolve_stop_files(stops, summary.as_deref(), &model) {
            Ok(files) => files,
            Err(text) => return McpResponse::Error(text),
        };

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
            about,
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

    /// Every stop's file, or the [`walkthrough_refusals`] as error text. An
    /// anchorless stop falls back to the first anchored stop's file, then
    /// `model`'s first file.
    fn resolve_stop_files(
        &self,
        stops: &[StopParams],
        summary: Option<&str>,
        model: &DiffModel,
    ) -> Result<Vec<String>, String> {
        let fallback = stops
            .iter()
            .filter_map(|stop| stop.anchor.as_deref())
            .map(|anchor| Target::parse(anchor).path().to_owned())
            .next()
            .or_else(|| model.files.first().map(|file| file.path.clone()));

        let refusals = walkthrough_refusals(
            stops,
            fallback.as_deref(),
            model,
            &self.review.repo_root,
            summary,
        );
        if !refusals.is_empty() {
            return Err(refusals
                .iter()
                .map(|receipt| {
                    let stop = receipt
                        .stop
                        .map_or("-".to_owned(), |index| index.to_string());
                    format!("stop {stop}: {}: {}", receipt.code.name(), receipt.detail)
                })
                .collect::<Vec<_>>()
                .join("\n"));
        }
        Ok(stops
            .iter()
            .map(|stop| {
                anchored_path(
                    stop.anchor.as_deref(),
                    fallback.as_deref().unwrap_or_default(),
                )
            })
            .collect())
    }

    fn review_summaries(&self) -> Vec<ReviewSummary> {
        self.review
            .all_reviews()
            .unwrap_or_default()
            .into_iter()
            .map(|(source, session)| {
                let (open, replied, resolved) = count_by_status(&session.comments);
                let label = session
                    .walkthrough
                    .as_ref()
                    .map_or_else(|| source.label(), |w| w.title.clone());
                ReviewSummary {
                    project: None,
                    source: source.key(),
                    label,
                    open_comments: open,
                    replied_comments: replied,
                    resolved_comments: resolved,
                }
            })
            .collect()
    }

    /// We cache pinned diffs (commit, range, PR) so agent polls never stall
    /// the render loop. Backend errors degrade to an empty diff.
    pub(crate) fn source_model(&mut self, source: &ReviewSource) -> std::sync::Arc<DiffModel> {
        match source {
            ReviewSource::WorkingTree => return std::sync::Arc::new(self.review.model().clone()),
            ReviewSource::Walkthrough { .. } => {
                let about = self.resolve_about(source);
                return self.source_model(&about);
            }
            // live like the working tree, so we never cache it
            ReviewSource::Against { rev } => {
                return std::sync::Arc::new(self.against_model_for(rev));
            }
            ReviewSource::Commit { .. } | ReviewSource::Range { .. } | ReviewSource::Pr { .. } => {}
        }
        let key = source.key();
        if !self.source_models.contains_key(&key) {
            let model = self.fetch_pinned(source).unwrap_or_default();
            self.source_models
                .insert(key.clone(), std::sync::Arc::new(model));
        }
        self.source_models.get(&key).cloned().unwrap_or_default()
    }

    /// The source whose diff `source` shows: a walkthrough resolves to the
    /// review it is about, every other source to itself.
    pub(crate) fn resolve_about(&mut self, source: &ReviewSource) -> ReviewSource {
        match source {
            ReviewSource::Walkthrough { id } => self.walkthrough_about(id),
            other => other.clone(),
        }
    }

    pub(crate) fn fetch_pinned(&self, source: &ReviewSource) -> Result<DiffModel, VcsError> {
        let pr_head = match source {
            ReviewSource::Pr { number } => Some(
                self.pr_ranges
                    .get(number)
                    .ok_or_else(|| VcsError::Rejected(format!("PR #{number} is not resolved")))?,
            ),
            _ => None,
        };
        diffler_core::review::pinned_diff(
            self.review.vcs.as_ref(),
            source,
            pr_head.map(|(base, head)| (base.as_str(), head.as_str())),
        )
    }

    fn comments_response(&mut self, keep: impl Fn(CommentStatus) -> bool) -> Vec<CommentInfo> {
        let mut out = Vec::new();
        for (source, session) in self.review.all_reviews().unwrap_or_default() {
            let comments: Vec<_> = session.comments.iter().filter(|c| keep(c.status)).collect();
            // a live source rebuilds its model here, so we skip sources with nothing to report
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

    /// Marks the comment replied; only the human resolves it (`R`). We post
    /// the note only when the agent has not replied yet, so it never repeats
    /// the answer.
    fn agent_propose_resolve(&mut self, id: &str, note: Option<&str>) -> McpResponse {
        let Some(source) = self.source_of_comment(id) else {
            return McpResponse::Error(format!("unknown comment id: {id}"));
        };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        let session = self.review.session_for_mut(&source);
        let answered = session.comment(id).is_some_and(|comment| {
            comment
                .replies
                .iter()
                .any(|reply| reply.author == AGENT_AUTHOR)
        });
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

    fn agent_add_comment(
        &mut self,
        file: &str,
        line: u32,
        line_end: Option<u32>,
        body: &str,
        as_human: bool,
    ) -> McpResponse {
        if let Some(end) = line_end
            && end < line
        {
            return McpResponse::Error(format!("line_end {end} is before line {line}"));
        }
        let body = body.trim();
        if body.is_empty() {
            return McpResponse::Error("comment body is empty".to_owned());
        }
        if body.len() > BODY_MAX_BYTES {
            return McpResponse::Error(format!(
                "comment body: {} bytes, {BODY_MAX_BYTES} at most",
                body.len()
            ));
        }
        let source = self.active_review_source();
        let model = self.source_model(&source);
        let anchor_line = line_end.unwrap_or(line);
        let Some((on_old_side, hunk, line_text)) = locate_anchor_line(&model, file, anchor_line)
        else {
            return McpResponse::Error(format!("{file}:{anchor_line} is not part of the diff"));
        };
        // a range spanning two hunks would cover lines the diff never touched
        if line != anchor_line
            && find_line_in_hunk(&model, file, line, on_old_side).map(|(index, _)| index)
                != Some(hunk)
        {
            return McpResponse::Error(format!("{file}:{line} is not part of the diff"));
        }
        let anchor = Anchor {
            file: file.to_owned(),
            line: Some(line),
            line_end,
            on_old_side,
            line_text: Some(line_text),
        };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        let author = if as_human {
            self.author.clone()
        } else {
            AGENT_AUTHOR.to_owned()
        };
        let id = self
            .review
            .session_for_mut(&source)
            .add_comment(anchor, &author, body)
            .id
            .clone();
        if let Err(err) = self.persist_review_change(&source) {
            return McpResponse::Error(err);
        }
        self.info(if as_human {
            format!("agent commented on {file} as you")
        } else {
            format!("agent commented on {file}")
        });
        McpResponse::Added { id }
    }

    /// Refuses another author's comment, and a walkthrough stop or note,
    /// since `publish_walkthrough` tracks those ids.
    fn check_own_editable_comment(&self, source: &ReviewSource, id: &str) -> Option<McpResponse> {
        let Some(comment) = self.review.session_for(source).comment(id) else {
            return Some(McpResponse::Error(format!("unknown comment id: {id}")));
        };
        if comment.author != AGENT_AUTHOR {
            return Some(McpResponse::Error(format!(
                "comment {id} is {}'s, not the agent's; only its own comments can be changed \
                 this way",
                comment.author
            )));
        }
        if comment.anchor_ref.is_some() {
            return Some(McpResponse::Error(
                "this comment is a walkthrough stop or note; publish_walkthrough manages those, \
                 revise or drop it there instead"
                    .to_owned(),
            ));
        }
        None
    }

    /// A reply lives inside its comment, so we refuse a delete that would
    /// take someone else's reply with it.
    fn check_no_foreign_reply(&self, source: &ReviewSource, id: &str) -> Option<McpResponse> {
        let comment = self.review.session_for(source).comment(id)?;
        let reply = comment.replies.iter().find(|r| r.author != AGENT_AUTHOR)?;
        Some(McpResponse::Error(format!(
            "comment {id} has a reply from {}; edit its body instead of deleting it",
            reply.author
        )))
    }

    fn agent_delete_comment(&mut self, id: &str) -> McpResponse {
        let Some(source) = self.source_of_comment(id) else {
            return McpResponse::Error(format!("unknown comment id: {id}"));
        };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        if let Some(response) = self.check_own_editable_comment(&source, id) {
            return response;
        }
        if let Some(response) = self.check_no_foreign_reply(&source, id) {
            return response;
        }
        self.review.session_for_mut(&source).delete_comment(id);
        if let Err(err) = self.persist_review_change(&source) {
            return McpResponse::Error(err);
        }
        self.info("agent deleted its own comment");
        McpResponse::Ok
    }

    fn agent_edit_comment(&mut self, id: &str, body: &str) -> McpResponse {
        let body = body.trim();
        if body.is_empty() {
            return McpResponse::Error("comment body is empty".to_owned());
        }
        if body.len() > BODY_MAX_BYTES {
            return McpResponse::Error(format!(
                "comment body: {} bytes, {BODY_MAX_BYTES} at most",
                body.len()
            ));
        }
        let Some(source) = self.source_of_comment(id) else {
            return McpResponse::Error(format!("unknown comment id: {id}"));
        };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        if let Some(response) = self.check_own_editable_comment(&source, id) {
            return response;
        }
        self.review.session_for_mut(&source).edit_comment(id, body);
        if let Err(err) = self.persist_review_change(&source) {
            return McpResponse::Error(err);
        }
        self.info("agent edited its own comment");
        McpResponse::Ok
    }
}

/// The side, hunk index and text of `line` in `file`, new side first.
fn locate_anchor_line(model: &DiffModel, file: &str, line: u32) -> Option<(bool, usize, String)> {
    if let Some((hunk, found)) = find_line_in_hunk(model, file, line, false) {
        return Some((false, hunk, found.text.clone()));
    }
    find_line_in_hunk(model, file, line, true).map(|(hunk, found)| (true, hunk, found.text.clone()))
}

fn find_line_in_hunk<'a>(
    model: &'a DiffModel,
    file: &str,
    line: u32,
    on_old_side: bool,
) -> Option<(usize, &'a diffler_core::model::DiffLine)> {
    let file = model.files.iter().find(|f| f.path == file)?;
    file.hunks.iter().enumerate().find_map(|(index, hunk)| {
        hunk.lines
            .iter()
            .find(|l| l.number_on(on_old_side) == Some(line))
            .map(|found| (index, found))
    })
}

fn anchored_path(anchor: Option<&str>, fallback: &str) -> String {
    anchor.map_or_else(
        || fallback.to_owned(),
        |anchor| Target::parse(anchor).path().to_owned(),
    )
}

fn notes_of(stop: &StopParams) -> impl Iterator<Item = &NoteParams> {
    stop.notes.iter().flatten()
}

/// Reuses the comment `id` names so a revision keeps its thread. Lines stay
/// unset until the anchor worker resolves `anchor_ref`.
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

/// A repeated id would overwrite the comment the first write just made, so
/// we refuse it.
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

/// A file in the diff or on disk. We reject an empty path, which a malformed
/// anchor like `"#foo"` parses to, since joining it yields the repo root.
fn file_in_review(path: &str, model: &DiffModel, repo_root: &Path) -> bool {
    !path.is_empty()
        && (model.files.iter().any(|f| f.path == path) || repo_root.join(path).exists())
}

/// `Ok(None)` is a stop with no anchor and no fallback, which the
/// publish-level `NothingToAnchor` receipt already reports.
fn stop_file_receipt(
    index: usize,
    anchor: Option<&str>,
    fallback: Option<&str>,
    model: &DiffModel,
    repo_root: &Path,
) -> Result<Option<String>, Receipt> {
    let Some(anchor) = anchor.map(str::trim) else {
        return Ok(fallback.map(str::to_owned));
    };
    if anchor.is_empty() {
        return Err(Receipt {
            stop: Some(index),
            code: ReceiptCode::AnchorUnparsed,
            detail: "anchor is empty; name a file, or omit it to use the walkthrough's own \
                     file"
                .to_owned(),
        });
    }
    let path = Target::parse(anchor).path().to_owned();
    if file_in_review(&path, model, repo_root) {
        Ok(Some(path))
    } else {
        Err(Receipt {
            stop: Some(index),
            code: ReceiptCode::AnchorFileMissing,
            detail: format!(
                "\"{anchor}\" names \"{path}\", not in this review: no such file in the \
                 diff or on disk; anchor a file that exists, or check it out first if you \
                 meant a different revision"
            ),
        })
    }
}

/// Adds every body length to `total`, the walkthrough's running byte count.
fn stop_and_note_refusals(
    index: usize,
    stop: &StopParams,
    fallback: Option<&str>,
    model: &DiffModel,
    repo_root: &Path,
    total: &mut usize,
) -> Vec<Receipt> {
    let mut receipts = Vec::new();
    *total += stop.body.len();
    if stop.body.len() > BODY_MAX_BYTES {
        receipts.push(Receipt {
            stop: Some(index),
            code: ReceiptCode::BodyTooLong,
            detail: format!(
                "{} bytes, {BODY_MAX_BYTES} at most; trim it and republish",
                stop.body.len()
            ),
        });
    }

    let stop_path = stop_file_receipt(index, stop.anchor.as_deref(), fallback, model, repo_root)
        .unwrap_or_else(|receipt| {
            receipts.push(receipt);
            None
        });

    for (at, note) in notes_of(stop).enumerate() {
        *total += note.body.len();
        if note.body.len() > BODY_MAX_BYTES {
            receipts.push(Receipt {
                stop: Some(index),
                code: ReceiptCode::BodyTooLong,
                detail: format!(
                    "note {at}: {} bytes, {BODY_MAX_BYTES} at most; trim it and republish",
                    note.body.len()
                ),
            });
        }
        let Some(note_anchor) = note.anchor.as_deref() else {
            continue;
        };
        let note_path = Target::parse(note_anchor).path().to_owned();
        if let Some(stop_path) = stop_path.as_deref()
            && stop_path != note_path
        {
            receipts.push(Receipt {
                stop: Some(index),
                code: ReceiptCode::NoteOutsideStop,
                detail: format!(
                    "note {at} names \"{note_path}\", stop {index} names \"{stop_path}\"; \
                     anchor the note anywhere in \"{stop_path}\", or give it its own stop"
                ),
            });
        }
    }
    receipts
}

/// Nothing is stored while any of these are present. `fallback` is the file
/// an anchorless stop uses.
fn walkthrough_refusals(
    stops: &[StopParams],
    fallback: Option<&str>,
    model: &DiffModel,
    repo_root: &Path,
    summary: Option<&str>,
) -> Vec<Receipt> {
    let mut receipts = Vec::new();
    if stops.is_empty() {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::EmptyStops,
            detail: "a walkthrough needs at least one stop; add one and republish".to_owned(),
        });
    } else if stops.len() > MAX_STOPS {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::TooManyStops,
            detail: format!(
                "{} stops, {MAX_STOPS} at most; drop some or split the change and republish",
                stops.len()
            ),
        });
    }
    if fallback.is_none() && stops.iter().any(|stop| stop.anchor.is_none()) {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::NothingToAnchor,
            detail: "no stop names a file and the open review has none; anchor every \
                     stop, or open the review this walkthrough describes before publishing"
                .to_owned(),
        });
    }
    receipts.extend(duplicate_id_receipts(stops));

    let mut total = summary.map_or(0, str::len);
    if let Some(summary) = summary.filter(|summary| summary.len() > BODY_MAX_BYTES) {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::BodyTooLong,
            detail: format!(
                "summary: {} bytes, {BODY_MAX_BYTES} at most; trim it and republish",
                summary.len()
            ),
        });
    }
    for (index, stop) in stops.iter().enumerate() {
        receipts.extend(stop_and_note_refusals(
            index, stop, fallback, model, repo_root, &mut total,
        ));
    }
    if total > TOTAL_MAX_BYTES {
        receipts.push(Receipt {
            stop: None,
            code: ReceiptCode::TotalTooLong,
            detail: format!(
                "{total} bytes total, {TOTAL_MAX_BYTES} at most; cut some stops or notes and republish"
            ),
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

/// Advisory receipts: whole-file anchors and simplified or dropped figures.
fn walkthrough_receipts(stops: &[StopParams]) -> Vec<ReceiptInfo> {
    let mut receipts = Vec::new();
    for (index, stop) in stops.iter().enumerate() {
        if let Some(anchor) = &stop.anchor
            && matches!(Target::parse(anchor), Target::File { .. })
        {
            receipts.push(ReceiptInfo {
                stop: Some(index),
                code: "anchor_whole".to_owned(),
                detail: format!(
                    "\"{anchor}\" has no symbol or line, so it anchors to the whole file"
                ),
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
    use crate::event::AppEvent;
    use crate::test_support::{standard_fixture, two_hunk_fixture};

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

    /// We observe the build through the cache, which a commit source fills
    /// the moment its model is built.
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

    #[test]
    fn add_comment_anchors_the_line_and_reads_outdated_like_any_comment() {
        let (_fixture, mut app, _human_id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "this looks wrong".to_owned(),
            as_human: false,
        });
        let McpResponse::Added { id } = response else {
            panic!("expected an added comment: {response:?}");
        };
        let comment = app.review.session.comment(&id).expect("comment stored");
        assert_eq!(comment.author, AGENT_AUTHOR);
        assert_eq!(comment.anchor.file, "src/lib.rs");
        assert_eq!(comment.anchor.line, Some(2));
        assert_eq!(comment.anchor.line_end, None);
        assert_eq!(comment.anchor.line_text.as_deref(), Some("    42"));
        assert!(!comment.anchor.on_old_side);
        assert!(!comment.anchor.is_outdated(app.review.model()));

        let mut drifted = app.review.model().clone();
        for line in drifted
            .files
            .iter_mut()
            .flat_map(|f| &mut f.hunks)
            .flat_map(|h| &mut h.lines)
        {
            if line.new_no == Some(2) {
                line.text = "changed since".to_owned();
            }
        }
        assert!(comment.anchor.is_outdated(&drifted));
    }

    #[test]
    fn add_comment_range_anchors_to_the_end_line() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 1,
            line_end: Some(3),
            body: "the whole function".to_owned(),
            as_human: false,
        });
        let McpResponse::Added { id } = response else {
            panic!("expected an added comment: {response:?}");
        };
        let comment = app.review.session.comment(&id).expect("comment stored");
        assert_eq!(comment.anchor.line, Some(1));
        assert_eq!(comment.anchor.line_end, Some(3));
        assert_eq!(comment.anchor.line_text.as_deref(), Some("}"));
    }

    #[test]
    fn add_comment_on_an_unknown_line_errors() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 999,
            line_end: None,
            body: "x".to_owned(),
            as_human: false,
        });
        assert!(matches!(response, McpResponse::Error(message) if message.contains("999")));
    }

    /// Line 10 sits between `two_hunk_fixture`'s hunks (1-4, 17-20).
    #[test]
    fn add_comment_range_with_a_start_outside_the_diff_errors() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let response = app.handle_mcp(McpRequestKind::AddComment {
            file: "data.txt".to_owned(),
            line: 10,
            line_end: Some(18),
            body: "x".to_owned(),
            as_human: false,
        });
        assert!(matches!(response, McpResponse::Error(message) if message.contains("data.txt:10")));
    }

    #[test]
    fn add_comment_range_spanning_two_hunks_errors() {
        let fixture = two_hunk_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let response = app.handle_mcp(McpRequestKind::AddComment {
            file: "data.txt".to_owned(),
            line: 2,
            line_end: Some(18),
            body: "x".to_owned(),
            as_human: false,
        });
        assert!(matches!(response, McpResponse::Error(message) if message.contains("data.txt:2")));
    }

    #[test]
    fn add_comment_with_an_empty_body_errors() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: String::new(),
            as_human: false,
        });
        assert!(matches!(response, McpResponse::Error(_)));
    }

    #[test]
    fn add_comment_with_a_whitespace_only_body_errors() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "   \n\t  ".to_owned(),
            as_human: false,
        });
        assert!(matches!(response, McpResponse::Error(_)));
    }

    #[test]
    fn add_comment_over_the_body_cap_errors() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "x".repeat(BODY_MAX_BYTES + 1),
            as_human: false,
        });
        assert!(matches!(response, McpResponse::Error(message) if message.contains("at most")));
    }

    #[test]
    fn add_comment_as_human_authors_it_with_the_human_name() {
        let (_fixture, mut app, _id) = app_with_comment();
        app.author = "matheus".to_owned();

        let McpResponse::Added { id: default_id } = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "agent's own".to_owned(),
            as_human: false,
        }) else {
            panic!("expected an added comment");
        };
        assert_eq!(
            app.review.session.comment(&default_id).unwrap().author,
            AGENT_AUTHOR
        );

        let McpResponse::Added { id: human_id } = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "on behalf of the human".to_owned(),
            as_human: true,
        }) else {
            panic!("expected an added comment");
        };
        assert_eq!(
            app.review.session.comment(&human_id).unwrap().author,
            "matheus"
        );
    }

    #[test]
    fn edit_comment_replaces_the_body_of_the_agents_own_comment() {
        let (_fixture, mut app, _human_id) = app_with_comment();
        let McpResponse::Added { id } = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "first guess".to_owned(),
            as_human: false,
        }) else {
            panic!("expected an added comment");
        };

        let response = app.handle_mcp(McpRequestKind::EditComment {
            id: id.clone(),
            body: "corrected".to_owned(),
        });
        assert!(matches!(response, McpResponse::Ok), "{response:?}");
        let comment = app.review.session.comment(&id).expect("comment stored");
        assert_eq!(comment.body, "corrected");
        assert_eq!(comment.author, AGENT_AUTHOR);
    }

    #[test]
    fn delete_comment_removes_the_agents_own_comment() {
        let (_fixture, mut app, _human_id) = app_with_comment();
        let McpResponse::Added { id } = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "drifted onto the wrong line".to_owned(),
            as_human: false,
        }) else {
            panic!("expected an added comment");
        };

        let response = app.handle_mcp(McpRequestKind::DeleteComment { id: id.clone() });
        assert!(matches!(response, McpResponse::Ok), "{response:?}");
        assert!(app.review.session.comment(&id).is_none());
    }

    #[test]
    fn delete_and_edit_comment_refuse_a_humans_own_comment() {
        let (_fixture, mut app, human_id) = app_with_comment();

        let edit = app.handle_mcp(McpRequestKind::EditComment {
            id: human_id.clone(),
            body: "rewritten by the agent".to_owned(),
        });
        assert!(matches!(edit, McpResponse::Error(_)), "{edit:?}");
        assert_eq!(
            app.review.session.comment(&human_id).unwrap().body,
            "why 42?",
            "the human's comment is untouched"
        );

        let delete = app.handle_mcp(McpRequestKind::DeleteComment {
            id: human_id.clone(),
        });
        assert!(matches!(delete, McpResponse::Error(_)), "{delete:?}");
        assert!(
            app.review.session.comment(&human_id).is_some(),
            "the human's comment survives"
        );
    }

    #[test]
    fn delete_and_edit_comment_refuse_a_walkthrough_stop() {
        let (_fixture, mut app, _human_id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("The answer", Some("src/lib.rs#answer"), "why 42")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let stop_id = stop_ids(&mut app, &published.id)
            .into_iter()
            .next()
            .expect("a stop");

        let edit = app.handle_mcp(McpRequestKind::EditComment {
            id: stop_id.clone(),
            body: "rewritten outside publish_walkthrough".to_owned(),
        });
        assert!(matches!(edit, McpResponse::Error(_)), "{edit:?}");

        let delete = app.handle_mcp(McpRequestKind::DeleteComment {
            id: stop_id.clone(),
        });
        assert!(matches!(delete, McpResponse::Error(_)), "{delete:?}");
        assert!(
            walkthrough_session(&mut app, &published.id)
                .comment(&stop_id)
                .is_some(),
            "the stop survives"
        );
    }

    #[test]
    fn deleting_an_agents_comment_with_a_humans_reply_is_refused() {
        let (_fixture, mut app, _human_comment_id) = app_with_comment();
        let McpResponse::Added { id } = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "this branch looks dead".to_owned(),
            as_human: false,
        }) else {
            panic!("expected an added comment");
        };
        assert!(app.review.session.reply(&id, "human", "no, main calls it"));

        let response = app.handle_mcp(McpRequestKind::DeleteComment { id: id.clone() });
        assert!(matches!(response, McpResponse::Error(_)), "{response:?}");
        let comment = app.review.session.comment(&id).expect("comment survives");
        assert_eq!(comment.replies.len(), 1, "the human's reply survives too");
        assert_eq!(comment.replies[0].body, "no, main calls it");

        let edit = app.handle_mcp(McpRequestKind::EditComment {
            id: id.clone(),
            body: "corrected: it is reachable from main".to_owned(),
        });
        assert!(matches!(edit, McpResponse::Ok), "{edit:?}");
    }

    #[test]
    fn deleting_an_agents_comment_with_only_its_own_reply_still_works() {
        let (_fixture, mut app, _human_comment_id) = app_with_comment();
        let McpResponse::Added { id } = app.handle_mcp(McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "checking this again".to_owned(),
            as_human: false,
        }) else {
            panic!("expected an added comment");
        };
        assert!(
            app.review
                .session
                .reply(&id, AGENT_AUTHOR, "confirmed, dropping it")
        );

        let response = app.handle_mcp(McpRequestKind::DeleteComment { id: id.clone() });
        assert!(matches!(response, McpResponse::Ok), "{response:?}");
        assert!(app.review.session.comment(&id).is_none());
    }

    #[test]
    fn edit_comment_on_an_unknown_id_errors() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::EditComment {
            id: "nope".to_owned(),
            body: "x".to_owned(),
        });
        assert!(matches!(response, McpResponse::Error(message) if message.contains("nope")));
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

    fn walkthrough_session<'a>(app: &'a mut App, id: &str) -> &'a diffler_core::session::Session {
        let source = ReviewSource::walkthrough(id);
        app.review.ensure_source(&source).expect("ensure source");
        app.review.session_for(&source)
    }

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

    #[test]
    fn publishing_with_nothing_else_open_describes_the_working_tree() {
        let (_fixture, mut app, _id) = app_with_comment();
        let McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop("The answer", Some("src/lib.rs#answer"), "why 42")],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };
        let walkthrough = walkthrough_session(&mut app, &published.id)
            .walkthrough
            .clone()
            .expect("walkthrough stored");
        assert_eq!(walkthrough.about, ReviewSource::WorkingTree);
    }

    /// A PR review open on a clean working tree, anchoring a file only the PR adds.
    #[test]
    fn publishing_while_a_pr_is_open_describes_and_later_renders_that_pr() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("base.rs", "pub fn base() {}\n");
        fixture.commit_all("base");
        fixture.branch("feature");
        fixture.checkout("feature");
        fixture.write("pr_only.rs", "pub fn only_in_pr() -> u32 {\n    7\n}\n");
        fixture.commit_all("add pr_only.rs");
        fixture.checkout("main");
        // `checkout` only moves HEAD, so we remove the feature file to leave the tree clean
        std::fs::remove_file(fixture.root.join("pr_only.rs")).expect("remove");

        let mut app = App::new(fixture.review(), LoadedConfig::default());
        assert!(
            app.review.model().files.is_empty(),
            "the working tree must be clean"
        );
        let base = app.review.vcs.resolve("main").expect("base");
        let head = app.review.vcs.resolve("feature").expect("head");
        app.open_pr_diff(7, &base, &head);

        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "add only_in_pr".to_owned(),
            stops: vec![stop("Only in the PR", Some("pr_only.rs#only_in_pr"), "why")],
            skipped: None,
            summary: None,
        });
        let McpResponse::WalkthroughPublished(published) = response else {
            panic!("expected a published walkthrough, not a refusal: {response:?}");
        };
        assert!(published.receipts.is_empty(), "{:?}", published.receipts);

        let walkthrough = walkthrough_session(&mut app, &published.id)
            .walkthrough
            .clone()
            .expect("walkthrough stored");
        assert_eq!(walkthrough.about, ReviewSource::pr(7));

        app.open_walkthrough_diff(&published.id);
        let diff = app.diff.as_ref().expect("the walkthrough opened");
        assert!(
            diff.model(&app.review)
                .files
                .iter()
                .any(|f| f.path == "pr_only.rs"),
            "the walkthrough renders the PR's diff, not the empty working tree"
        );
    }

    fn github_ci_remote() -> crate::app::CiRemote {
        crate::app::CiRemote {
            name: "origin".into(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::GitHub,
                host: None,
            },
            url: None,
        }
    }

    /// A walkthrough about a PR, opened after a restart with nothing about
    /// that PR resolved yet.
    #[test]
    fn opening_a_walkthrough_about_a_pr_resolves_it_with_nothing_fetched_yet() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("base.rs", "pub fn base() {}\n");
        fixture.commit_all("base");
        fixture.branch("feature");
        fixture.checkout("feature");
        fixture.write("pr_only.rs", "pub fn only_in_pr() -> u32 {\n    7\n}\n");
        fixture.commit_all("add pr_only.rs");
        fixture.checkout("main");
        std::fs::remove_file(fixture.root.join("pr_only.rs")).expect("remove");

        let published_id = {
            let mut app = App::new(fixture.review(), LoadedConfig::default());
            let base = app.review.vcs.resolve("main").expect("base");
            let head = app.review.vcs.resolve("feature").expect("head");
            app.open_pr_diff(7, &base, &head);
            let McpResponse::WalkthroughPublished(published) =
                app.handle_mcp(McpRequestKind::PublishWalkthrough {
                    id: None,
                    title: "add only_in_pr".to_owned(),
                    stops: vec![stop("Only in the PR", Some("pr_only.rs#only_in_pr"), "why")],
                    skipped: None,
                    summary: None,
                })
            else {
                panic!("expected a published walkthrough");
            };
            published.id
        };
        let head_oid = fixture.review().vcs.resolve("feature").expect("head");

        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.ci_remotes = vec![github_ci_remote()];
        assert!(app.pr_ranges.is_empty(), "nothing resolved yet");
        assert!(app.pr.is_none());
        assert!(app.prs.is_empty());

        app.open_walkthrough(&published_id, crate::app::diff::Slide::Stop(0));
        assert!(
            app.diff.is_none(),
            "the PR isn't known yet: the walkthrough waits rather than opening empty"
        );
        assert!(
            matches!(app.pending_ci, Some(crate::app::CiRequest::Prs)),
            "{:?}",
            app.pending_ci
        );
        assert_eq!(
            app.pending_walkthrough_open
                .as_ref()
                .map(|(id, _)| id.clone()),
            Some(published_id.clone())
        );

        app.on_prs_event(vec![crate::ci::PullRequest {
            number: 7,
            title: "add pr_only.rs".into(),
            url: None,
            base_ref: "main".into(),
            head_ref: "feature".into(),
            head_oid,
            author: "reviewer".into(),
        }]);

        assert!(
            app.pending_walkthrough_open.is_none(),
            "the retry consumes the stashed open"
        );
        let diff = app
            .diff
            .as_ref()
            .expect("the walkthrough opens once the PR resolves");
        assert!(
            diff.model(&app.review)
                .files
                .iter()
                .any(|f| f.path == "pr_only.rs"),
            "the walkthrough renders the PR's diff, not an empty one"
        );
    }

    #[test]
    fn opening_a_walkthrough_about_a_known_pr_fetches_its_head_first() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("base.rs", "pub fn base() {}\n");
        fixture.commit_all("base");
        fixture.branch("feature");
        fixture.checkout("feature");
        fixture.write("pr_only.rs", "pub fn only_in_pr() -> u32 {\n    7\n}\n");
        fixture.commit_all("add pr_only.rs");
        fixture.checkout("main");
        std::fs::remove_file(fixture.root.join("pr_only.rs")).expect("remove");

        let published_id = {
            let mut app = App::new(fixture.review(), LoadedConfig::default());
            let base = app.review.vcs.resolve("main").expect("base");
            let head = app.review.vcs.resolve("feature").expect("head");
            app.open_pr_diff(7, &base, &head);
            let McpResponse::WalkthroughPublished(published) =
                app.handle_mcp(McpRequestKind::PublishWalkthrough {
                    id: None,
                    title: "add only_in_pr".to_owned(),
                    stops: vec![stop("Only in the PR", Some("pr_only.rs#only_in_pr"), "why")],
                    skipped: None,
                    summary: None,
                })
            else {
                panic!("expected a published walkthrough");
            };
            published.id
        };
        let head_oid = fixture.review().vcs.resolve("feature").expect("head");

        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.ci_remotes = vec![github_ci_remote()];
        app.prs = vec![crate::ci::PullRequest {
            number: 7,
            title: "add pr_only.rs".into(),
            url: None,
            base_ref: "not-fetched-yet".into(),
            head_ref: "feature".into(),
            head_oid,
            author: "reviewer".into(),
        }];

        app.open_walkthrough(&published_id, crate::app::diff::Slide::Stop(0));
        assert!(
            app.diff.is_none(),
            "the base isn't local yet: waits rather than opening empty"
        );
        let git = app.pending_git.take().expect("a fetch is queued");
        assert!(
            git.argv.iter().any(|a| a == "refs/pull/7/head"),
            "{:?}",
            git.argv
        );
        assert_eq!(app.pending_pr_open.as_ref().map(|pr| pr.number), Some(7));
        assert!(app.pending_walkthrough_open.is_some());

        fixture.branch("not-fetched-yet");
        app.handle(AppEvent::GitDone {
            label: App::pr_fetch_label(7),
            ok: true,
            output: String::new(),
        });

        let diff = app
            .diff
            .as_ref()
            .expect("the walkthrough opens once the fetch lands");
        assert!(
            diff.model(&app.review)
                .files
                .iter()
                .any(|f| f.path == "pr_only.rs"),
            "the walkthrough renders the PR's diff, not an empty one"
        );
    }

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

    /// An agent's id is arbitrary text, so a `/` in it must not become a path.
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

    /// Anchorless stops on a clean working tree have no file to fall back on.
    #[test]
    fn publishing_with_no_anchors_against_an_empty_diff_is_refused() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("a.rs", "pub fn a() {}\n");
        fixture.commit_all("initial commit");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        assert!(
            app.review.model().files.is_empty(),
            "the fixture must start with nothing in the diff"
        );

        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![stop("one", None, "why")],
            skipped: None,
            summary: None,
        });
        let McpResponse::Error(text) = response else {
            panic!("expected a refusal: {response:?}");
        };
        assert!(text.contains("nothing_to_anchor"), "{text}");
        assert!(
            app.review
                .all_reviews()
                .expect("all reviews")
                .into_iter()
                .all(
                    |(s, session)| !matches!(s, ReviewSource::Walkthrough { .. })
                        || session.walkthrough.is_none()
                ),
            "a refused walkthrough is never stored, and never with an empty anchor"
        );
    }

    #[test]
    fn a_stop_anchored_to_a_file_outside_the_review_is_refused() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![stop("nowhere", Some("does/not/exist.rs"), "why")],
            skipped: None,
            summary: None,
        });
        let McpResponse::Error(text) = response else {
            panic!("expected a refusal: {response:?}");
        };
        assert!(text.contains("anchor_file_missing"), "{text}");
        assert!(text.contains("does/not/exist.rs"), "{text}");
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

    /// `notes.txt` is committed and unchanged, so only the disk has it.
    #[test]
    fn a_stop_anchored_outside_the_diff_but_on_disk_still_publishes() {
        let (_fixture, mut app, _id) = app_with_comment();
        assert!(
            !app.review
                .model()
                .files
                .iter()
                .any(|f| f.path == "notes.txt"),
            "notes.txt must not be part of the diff for this to test the context-file case"
        );
        let response = app.handle_mcp(McpRequestKind::PublishWalkthrough {
            id: None,
            title: "tour".to_owned(),
            stops: vec![
                stop("The answer", Some("src/lib.rs#answer"), "why 42"),
                stop("Some context", Some("notes.txt"), "unrelated but real"),
            ],
            skipped: None,
            summary: None,
        });
        let McpResponse::WalkthroughPublished(published) = response else {
            panic!("expected a published walkthrough: {response:?}");
        };
        assert_eq!(published.stops, 2);
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
    fn a_class_diagram_fence_yields_a_figure_dropped_receipt() {
        let (_fixture, mut app, _id) = app_with_comment();
        let body = "```mermaid\nclassDiagram\n  Animal <|-- Dog\n```\n";
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
    fn a_sequence_diagram_fence_publishes_with_no_dropped_receipt() {
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
            !published
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

    #[test]
    fn get_walkthrough_by_an_unknown_id_returns_none_not_an_error() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::GetWalkthrough {
            id: Some("nope".to_owned()),
        });
        assert_eq!(response, McpResponse::Walkthrough(None));
    }

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
        // both publishes can share a timestamp, so we force them apart
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
        // both publishes can share a timestamp, so we force them apart
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
            project: None,
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

    #[test]
    fn every_tool_call_maps_itself_to_an_activity_phrase() {
        let (_fixture, mut app, _id) = app_with_comment();
        assert!(app.agent_activity.current.is_none());

        app.handle_mcp(McpRequestKind::GetDiff {
            file: Some("src/lib.rs".to_owned()),
        });
        let activity = app.agent_activity.current.as_ref().expect("activity set");
        assert_eq!(activity.focus, "reading the diff");
        assert_eq!(activity.file.as_deref(), Some("src/lib.rs"));

        app.handle_mcp(McpRequestKind::ListReviews);
        let activity = app.agent_activity.current.as_ref().expect("activity set");
        assert_eq!(activity.focus, "listing reviews");
        assert_eq!(activity.file, None, "no call names a file for this one");
    }

    #[test]
    fn report_activity_overrides_the_generic_phrase() {
        let (_fixture, mut app, _id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::ReportActivity {
            focus: "writing the walkthrough".to_owned(),
            file: Some("src/app/refresh.rs".to_owned()),
        });
        assert_eq!(response, McpResponse::Ok);
        let activity = app.agent_activity.current.as_ref().expect("activity set");
        assert_eq!(activity.focus, "writing the walkthrough");
        assert_eq!(activity.file.as_deref(), Some("src/app/refresh.rs"));

        app.handle_mcp(McpRequestKind::ListReviews);
        let activity = app.agent_activity.current.as_ref().expect("activity set");
        assert_eq!(activity.file, None);
    }
}
