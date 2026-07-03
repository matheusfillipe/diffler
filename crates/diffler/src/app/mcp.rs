//! App-side handling of agent tool calls. Runs synchronously on the main
//! loop against the owned review state; the `mcp` module only ships
//! requests here and renders the responses.

use diffler_core::model::DiffModel;
use diffler_core::session::CommentStatus;
use diffler_core::source::ReviewSource;

use super::App;
use crate::mcp::{
    AGENT_AUTHOR, CommentInfo, FileEntry, McpRequestKind, McpResponse, ReviewStatusResponse,
    ReviewSummary, comment_info, comment_status_name, file_status_name, render_unified,
};

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
            McpRequestKind::Feedback => {
                McpResponse::Comments(self.comments_response(|c| c != CommentStatus::Resolved))
            }
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
        }
    }

    fn review_summaries(&self) -> Vec<ReviewSummary> {
        self.review
            .all_reviews()
            .unwrap_or_default()
            .into_iter()
            .map(|(source, session)| {
                let (open, replied, resolved) = count_by_status(&session.comments);
                ReviewSummary {
                    source: source.key(),
                    label: source.label(),
                    open_comments: open,
                    replied_comments: replied,
                    resolved_comments: resolved,
                }
            })
            .collect()
    }

    /// The diff a source is reviewing, used to render comment context and judge
    /// outdated-ness. Commit and range diffs are immutable, so they compute
    /// once and stay cached — agent polls must not stall the render loop.
    /// Backend errors degrade to an empty diff.
    pub(crate) fn source_model(&mut self, source: &ReviewSource) -> std::sync::Arc<DiffModel> {
        let key = match source {
            ReviewSource::WorkingTree => {
                return std::sync::Arc::new(self.review.model().clone());
            }
            ReviewSource::Commit { .. } | ReviewSource::Range { .. } | ReviewSource::Pr { .. } => {
                source.key()
            }
        };
        if !self.source_models.contains_key(&key) {
            let model = match source {
                ReviewSource::WorkingTree => DiffModel::default(),
                ReviewSource::Commit { oid } => {
                    self.review.vcs.commit_diff(oid).unwrap_or_default()
                }
                ReviewSource::Range { oldest, newest } => self
                    .review
                    .vcs
                    .range_diff(oldest, newest)
                    .unwrap_or_default(),
                // resolved when the PR view opened; unknown PRs degrade empty
                ReviewSource::Pr { number } => self
                    .pr_ranges
                    .get(number)
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
            let model = self.source_model(&source);
            for comment in session.comments.iter().filter(|c| keep(c.status)) {
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
        self.persist_agent_change(&source);
        self.info("agent replied to a comment");
        self.comment_status_response(&source, id)
    }

    /// The agent can only propose: the comment moves to replied with a
    /// flagged note, and the human resolves it in the TUI (`R`).
    fn agent_propose_resolve(&mut self, id: &str, note: Option<&str>) -> McpResponse {
        let Some(source) = self.source_of_comment(id) else {
            return McpResponse::Error(format!("unknown comment id: {id}"));
        };
        if let Err(err) = self.review.ensure_source(&source) {
            return McpResponse::Error(err.to_string());
        }
        let note = note
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("marked resolved");
        let body = format!("[agent] {note}");
        self.review
            .session_for_mut(&source)
            .reply(id, AGENT_AUTHOR, &body);
        self.persist_agent_change(&source);
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
        self.persist_agent_change(&source);
        self.info(format!("agent marked {file} viewed"));
        McpResponse::Ok
    }

    /// Persist one source after an agent mutation and refresh the open diff.
    /// Unlike the human path this never bumps the feedback epoch — an agent's
    /// own change must not wake its `wait_for_feedback` poll.
    fn persist_agent_change(&mut self, source: &ReviewSource) {
        if let Err(err) = self.review.save_for(source) {
            self.error(err.to_string());
        }
        if let Some(diff) = self.diff.as_mut() {
            diff.invalidate();
        }
    }
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
                "human",
                Anchor {
                    file: "src/lib.rs".to_owned(),
                    line: Some(2),
                    line_end: None,
                    on_old_side: false,
                    line_text: Some("    42".to_owned()),
                },
                "why 42?",
            )
            .id
            .clone();
        (fixture, app, id)
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
        let McpResponse::Comments(before_feedback) = app.handle_mcp(McpRequestKind::Feedback)
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
        let McpResponse::Comments(after_feedback) = app.handle_mcp(McpRequestKind::Feedback) else {
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
    fn propose_resolve_appends_flagged_note_and_stays_replied() {
        let (_fixture, mut app, id) = app_with_comment();
        let response = app.handle_mcp(McpRequestKind::ProposeResolve {
            id: id.clone(),
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
        assert_eq!(comment.replies[0].body, "[agent] fixed in abc123");

        let response = app.handle_mcp(McpRequestKind::ProposeResolve { id, note: None });
        assert!(matches!(response, McpResponse::Replied { .. }));
        let comment = &app.review.session.comments[0];
        assert_eq!(comment.replies[1].body, "[agent] marked resolved");
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
            .add_comment("human", commit_anchor("src/lib.rs"), "on the commit")
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
            .add_comment("human", commit_anchor("src/lib.rs"), "why here?")
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
            .add_comment("human", commit_anchor("src/lib.rs"), "x");
        app.review.save_for(&source).expect("save");

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
            "human",
            Anchor {
                file: "todo.md".to_owned(),
                line: None,
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "second",
        );
        app.review.session.resolve(&id);
        let McpResponse::Comments(comments) = app.handle_mcp(McpRequestKind::Feedback) else {
            panic!("expected comments");
        };
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].body, "second");
    }
}
