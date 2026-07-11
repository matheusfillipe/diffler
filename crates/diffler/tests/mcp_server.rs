//! Headless full-stack MCP coverage: a real git fixture, the real `App`
//! event pump (replicating the main-loop dispatch), the real axum/rmcp
//! server on an ephemeral port, and the rmcp client over streamable HTTP.

// helper fns run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]

mod common;

use common::{Fixture, fixture};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use diffler::app::{App, Flow};
use diffler::config::LoadedConfig;
use diffler::event::AppEvent;
use diffler::mcp;
use diffler_core::review::Review;
use diffler_core::session::Anchor;
use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientInfo};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{Value, json};
use tokio::sync::mpsc::{self, UnboundedSender};

fn anchor_on_line_two() -> Anchor {
    Anchor {
        file: "src/lib.rs".to_owned(),
        line: Some(2),
        line_end: None,
        on_old_side: false,
        line_text: Some("    42".to_owned()),
    }
}

type McpClient = rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>;

struct Harness {
    _fixture: Fixture,
    tx: UnboundedSender<AppEvent>,
    client: McpClient,
}

/// Build the app, seed it, spawn the event pump exactly like the main loop
/// (recv → `App::handle`), serve MCP on an ephemeral port, and connect.
async fn start(seed: impl FnOnce(&mut App)) -> Harness {
    let fixture = fixture();
    let review = Review::open(&fixture.root).expect("review");
    let mut app = App::new(review, LoadedConfig::default());
    "human".clone_into(&mut app.author);
    seed(&mut app);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let feedback_rx = app.feedback_tx.subscribe();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if app.handle(event) == Flow::Quit {
                break;
            }
        }
    });
    let handle = mcp::spawn_mcp(tx.clone(), feedback_rx, 0).expect("mcp server");

    let transport =
        StreamableHttpClientTransport::from_uri(format!("http://127.0.0.1:{}/mcp", handle.port));
    let client = ClientInfo::default()
        .serve(transport)
        .await
        .expect("client");
    Harness {
        _fixture: fixture,
        tx,
        client,
    }
}

async fn call(client: &McpClient, tool: &str, args: Value) -> CallToolResult {
    let arguments = match args {
        Value::Object(map) => Some(map),
        _ => None,
    };
    let mut params = CallToolRequestParams::new(tool.to_owned());
    if let Some(arguments) = arguments {
        params = params.with_arguments(arguments);
    }
    client.call_tool(params).await.expect("tool call")
}

fn structured(result: &CallToolResult) -> &Value {
    result
        .structured_content
        .as_ref()
        .expect("structured content")
}

fn send_key(tx: &UnboundedSender<AppEvent>, c: char) {
    let modifiers = if c.is_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    tx.send(AppEvent::Key(KeyEvent::new(KeyCode::Char(c), modifiers)))
        .expect("send key");
}

#[tokio::test]
async fn review_status_reflects_the_fixture() {
    let harness = start(|_| {}).await;
    let result = call(&harness.client, "review_status", Value::Null).await;
    let status = structured(&result);
    assert_eq!(status["repo"], "fixture");
    assert_eq!(status["branch"], "main");
    assert_eq!(status["oid7"].as_str().expect("oid7").len(), 7);
    assert_eq!(status["files_changed"][0]["path"], "src/lib.rs");
    assert_eq!(status["files_changed"][0]["status"], "modified");
    assert_eq!(status["files_changed"][0]["viewed"], false);
    assert_eq!(status["open_comments"], 0);
    assert_eq!(status["feedback_epoch"], 0);
}

#[tokio::test]
async fn get_diff_returns_unified_text_and_rejects_unknown_files() {
    let harness = start(|_| {}).await;
    let result = call(&harness.client, "get_diff", json!({ "file": "src/lib.rs" })).await;
    let diff = structured(&result)["diff"].as_str().expect("diff text");
    assert!(diff.contains("--- a/src/lib.rs"));
    assert!(diff.contains("+++ b/src/lib.rs"));
    assert!(diff.contains("-    41"));
    assert!(diff.contains("+    42"));

    let err = harness
        .client
        .call_tool(
            CallToolRequestParams::new("get_diff").with_arguments(
                json!({ "file": "nope.rs" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        )
        .await;
    assert!(err.is_err(), "unknown file must be a tool error: {err:?}");
}

#[tokio::test]
async fn comment_lifecycle_reply_resolve_and_viewed() {
    let harness = start(|app| {
        app.review
            .session
            .add_comment(anchor_on_line_two(), "human", "why 42?");
        app.review.save().expect("save");
    })
    .await;

    // the human's comment arrives with anchor + context
    let result = call(&harness.client, "get_comments", json!({})).await;
    let comments = structured(&result)["comments"]
        .as_array()
        .expect("comments")
        .clone();
    assert_eq!(comments.len(), 1);
    let comment = &comments[0];
    assert_eq!(comment["file"], "src/lib.rs");
    assert_eq!(comment["line"], 2);
    assert_eq!(comment["side"], "new");
    assert_eq!(comment["status"], "open");
    assert_eq!(comment["body"], "why 42?");
    assert_eq!(comment["outdated"], false);
    let context = comment["context"].as_str().expect("context");
    assert!(context.contains("-    41"));
    assert!(context.contains("+    42"));
    let id = comment["id"].as_str().expect("id").to_owned();

    // agent reply flips the status to replied
    let result = call(
        &harness.client,
        "reply_comment",
        json!({ "id": id, "body": "it is the answer" }),
    )
    .await;
    assert_eq!(structured(&result)["ok"], true);
    assert_eq!(structured(&result)["status"], "replied");

    // propose_resolve appends a flagged agent note, stays replied
    let result = call(
        &harness.client,
        "propose_resolve",
        json!({ "id": id, "note": "fixed upstream" }),
    )
    .await;
    assert_eq!(structured(&result)["status"], "replied");
    let result = call(
        &harness.client,
        "get_comments",
        json!({ "status": "replied" }),
    )
    .await;
    let comments = &structured(&result)["comments"];
    assert_eq!(comments[0]["replies"][0]["author"], "agent");
    assert_eq!(comments[0]["replies"][0]["body"], "it is the answer");
    assert_eq!(comments[0]["replies"][1]["body"], "[agent] fixed upstream");

    // mark_viewed is reflected in review_status
    let result = call(
        &harness.client,
        "mark_viewed",
        json!({ "file": "src/lib.rs" }),
    )
    .await;
    assert_eq!(structured(&result)["ok"], true);
    let result = call(&harness.client, "review_status", Value::Null).await;
    let status = structured(&result);
    assert_eq!(status["files_changed"][0]["viewed"], true);
    assert_eq!(status["open_comments"], 0);
    assert_eq!(status["replied_comments"], 1);

    // unknown ids are tool errors
    let err = harness
        .client
        .call_tool(
            CallToolRequestParams::new("reply_comment").with_arguments(
                json!({ "id": "nope", "body": "?" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        )
        .await;
    assert!(err.is_err(), "unknown id must be a tool error: {err:?}");
}

// `parse_status` lives in the tool layer, so only a real client call
// exercises the rejection.
#[tokio::test]
async fn get_comments_with_invalid_status_is_a_tool_error() {
    let harness = start(|_| {}).await;
    let err = harness
        .client
        .call_tool(
            CallToolRequestParams::new("get_comments").with_arguments(
                json!({ "status": "bogus" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        )
        .await;
    assert!(err.is_err(), "invalid status must be a tool error: {err:?}");
}

#[tokio::test]
async fn comment_payloads_carry_range_and_old_side_anchors() {
    let harness = start(|app| {
        // range comment over the whole function on the new side; the
        // anchor end (line 3, "}") is what outdated detection checks
        app.review.session.add_comment(
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(1),
                line_end: Some(3),
                on_old_side: false,
                line_text: Some("}".to_owned()),
            },
            "human",
            "whole function",
        );
        // single-line comment on the deleted side (old line 2, "    41")
        app.review.session.add_comment(
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: Some(2),
                line_end: None,
                on_old_side: true,
                line_text: Some("    41".to_owned()),
            },
            "human",
            "why drop 41?",
        );
    })
    .await;

    let result = call(&harness.client, "get_comments", json!({})).await;
    let comments = structured(&result)["comments"]
        .as_array()
        .expect("comments")
        .clone();
    assert_eq!(comments.len(), 2);
    let by_body = |body: &str| {
        comments
            .iter()
            .find(|c| c["body"] == body)
            .expect("comment present")
    };

    let range = by_body("whole function");
    assert_eq!(range["line"], 1);
    assert_eq!(range["line_end"], 3);
    assert_eq!(range["side"], "new");
    assert_eq!(range["outdated"], false);

    let old_side = by_body("why drop 41?");
    assert_eq!(old_side["line"], 2);
    assert_eq!(old_side["line_end"], Value::Null);
    assert_eq!(old_side["side"], "old");
    assert_eq!(old_side["outdated"], false);
    let context = old_side["context"].as_str().expect("context");
    assert!(context.contains("-    41"), "old-side context: {context}");
    assert!(context.contains("+    42"), "old-side context: {context}");
}

#[tokio::test]
async fn wait_for_feedback_unblocks_on_the_send_key() {
    let harness = start(|app| {
        app.review
            .session
            .add_comment(anchor_on_line_two(), "human", "why 42?");
    })
    .await;

    let waiter = {
        let client = std::sync::Arc::new(harness.client);
        let cloned = std::sync::Arc::clone(&client);
        let task = tokio::spawn(async move {
            cloned
                .call_tool(
                    CallToolRequestParams::new("wait_for_feedback").with_arguments(
                        json!({ "since_epoch": 0, "timeout_seconds": 30 })
                            .as_object()
                            .expect("object")
                            .clone(),
                    ),
                )
                .await
                .expect("wait_for_feedback")
        });
        (client, task)
    };
    let (client, task) = waiter;

    // no signal marks when the long poll has reached the server and
    // subscribed, so poll repeatedly over a generous window instead of
    // guessing one delay: any iteration seeing it finished early fails fast
    for _ in 0..40 {
        assert!(!task.is_finished(), "the poll must block until the bump");
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    send_key(&harness.tx, 'Z');

    let result = task.await.expect("join");
    let payload = structured(&result);
    assert_eq!(payload["timed_out"], false);
    assert_eq!(payload["epoch"], 1);
    let comments = payload["comments"].as_array().expect("comments");
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["body"], "why 42?");
    drop(client);
}

#[tokio::test]
async fn wait_for_feedback_times_out_without_feedback() {
    let harness = start(|_| {}).await;
    let result = call(
        &harness.client,
        "wait_for_feedback",
        json!({ "timeout_seconds": 1 }),
    )
    .await;
    let payload = structured(&result);
    assert_eq!(payload["timed_out"], true);
    assert_eq!(payload["epoch"], 0);
}
