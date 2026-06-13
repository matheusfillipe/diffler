//! App-side handling of agent tool calls. Runs synchronously on the main
//! loop against the owned review state; the `mcp` module only ships
//! requests here and renders the responses.

use diffler_core::session::CommentStatus;

use super::App;
use crate::mcp::{
    AGENT_AUTHOR, CommentInfo, FileEntry, McpRequestKind, McpResponse, ReviewStatusResponse,
    comment_info, comment_status_name, file_status_name, render_unified,
};

impl App {
    pub(crate) fn handle_mcp(&mut self, kind: McpRequestKind) -> McpResponse {
        match kind {
            McpRequestKind::ReviewStatus => McpResponse::Status(self.review_status_response()),
            McpRequestKind::GetDiff { file } => {
                match render_unified(&self.review.model, file.as_deref()) {
                    Ok(diff) => McpResponse::Diff(diff),
                    Err(message) => McpResponse::Error(message),
                }
            }
            McpRequestKind::GetComments { status } => McpResponse::Comments(
                self.comments_response(|c| status.is_none_or(|wanted| c == wanted)),
            ),
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
            .model
            .files
            .iter()
            .map(|f| FileEntry {
                path: f.path.clone(),
                status: file_status_name(f.status).to_owned(),
                viewed: self.review.session.is_viewed(&f.path, &f.content_hash()),
            })
            .collect();
        let (mut open, mut replied, mut resolved) = (0, 0, 0);
        for comment in &self.review.session.comments {
            match comment.status {
                CommentStatus::Open => open += 1,
                CommentStatus::Replied => replied += 1,
                CommentStatus::Resolved => resolved += 1,
            }
        }
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
        }
    }

    fn comments_response(&self, keep: impl Fn(CommentStatus) -> bool) -> Vec<CommentInfo> {
        self.review
            .session
            .comments
            .iter()
            .filter(|c| keep(c.status))
            .map(|c| comment_info(c, &self.review.model))
            .collect()
    }

    fn agent_reply(&mut self, id: &str, body: &str) -> McpResponse {
        if !self.review.session.reply(id, AGENT_AUTHOR, body) {
            return McpResponse::Error(format!("unknown comment id: {id}"));
        }
        self.after_agent_session_change();
        self.info("agent replied to a comment");
        self.comment_status_response(id)
    }

    /// The agent can only propose: the comment moves to replied with a
    /// flagged note, and the human resolves it in the TUI (`R`).
    fn agent_propose_resolve(&mut self, id: &str, note: Option<&str>) -> McpResponse {
        let note = note
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("marked resolved");
        let body = format!("[agent] {note}");
        if !self.review.session.reply(id, AGENT_AUTHOR, &body) {
            return McpResponse::Error(format!("unknown comment id: {id}"));
        }
        self.after_agent_session_change();
        self.info("agent proposed resolving a comment (confirm with R)");
        self.comment_status_response(id)
    }

    fn comment_status_response(&self, id: &str) -> McpResponse {
        let status = self
            .review
            .session
            .comments
            .iter()
            .find(|c| c.id == id)
            .map_or(CommentStatus::Open, |c| c.status);
        McpResponse::Replied {
            status: comment_status_name(status).to_owned(),
        }
    }

    fn agent_mark_viewed(&mut self, file: &str) -> McpResponse {
        let Some(hash) = self
            .review
            .model
            .files
            .iter()
            .find(|f| f.path == file)
            .map(diffler_core::model::FileDiff::content_hash)
        else {
            return McpResponse::Error(format!("unknown file: {file}"));
        };
        self.review.session.mark_viewed(file, &hash);
        self.after_agent_session_change();
        self.info(format!("agent marked {file} viewed"));
        McpResponse::Ok
    }
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
                    hunk: None,
                    line_text: Some("    42".to_owned()),
                },
                "why 42?",
            )
            .id
            .clone();
        (fixture, app, id)
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
                hunk: None,
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
