//! Tests for comment cards, threads, selection comments and their lifecycle.

use super::*;

#[test]
fn brace_keys_walk_between_comments() {
    let fixture = two_hunk_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    let anchor = |line: u32| Anchor {
        file: "data.txt".to_owned(),
        line: Some(line),
        line_end: None,
        on_old_side: false,
        line_text: None,
    };
    app.review.session.add_comment(anchor(1), "r", "first");
    app.review.session.add_comment(anchor(20), "r", "second");
    app.open_working_tree_diff(None);
    {
        let diff = app.diff.as_mut().unwrap();
        diff.focus = Pane::Diff;
        diff.cursor = 0;
        diff.invalidate();
    }
    app.diff.as_mut().unwrap().ensure_rows(&app.review);

    let on_header = |app: &App| {
        let diff = app.diff.as_ref().unwrap();
        matches!(
            diff.rows().get(diff.cursor),
            Some(DiffRow::Comment { line: 0, .. })
        )
    };
    app.handle(key('}'));
    assert!(on_header(&app), "}} lands on a comment header");
    let first = app.diff.as_ref().unwrap().cursor;
    app.handle(key('}'));
    assert!(on_header(&app));
    let second = app.diff.as_ref().unwrap().cursor;
    assert!(second > first, "}} advances to the next comment");
    app.handle(key('{'));
    assert_eq!(
        app.diff.as_ref().unwrap().cursor,
        first,
        "{{ returns to the previous comment"
    );
}

#[test]
fn comment_rows_appear_under_their_anchored_line() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    let position = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = position;
    app.handle(key('c'));
    assert!(app.composer_open());
    type_text(&mut app, "why 42?");
    app.handle(key('\n'));
    assert!(!app.composer_open());

    let comment = &app.review.session.comments[0];
    assert_eq!(comment.author, "reviewer");
    assert_eq!(comment.body, "why 42?");
    assert_eq!(comment.anchor.file, "src/lib.rs");
    assert_eq!(comment.anchor.line, Some(2));
    assert!(!comment.anchor.on_old_side);
    assert_eq!(comment.anchor.line_text.as_deref(), Some("    42"));

    let diff = app.diff.as_mut().unwrap();
    diff.ensure_rows(&app.review);
    let rows = rows(&app);
    let line_position = added_line_position(&app);
    let block: Vec<_> = rows
        .iter()
        .skip(line_position + 1)
        .take_while(|r| matches!(r, DiffRow::Comment { .. }))
        .collect();
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
    select_file(&mut app, "src/lib.rs");
    app.review.session.add_comment(
        Anchor {
            file: "src/lib.rs".to_owned(),
            line: Some(2),
            line_end: None,
            on_old_side: false,
            line_text: Some("    43".to_owned()),
        },
        "reviewer",
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
fn comment_for_a_departed_line_attaches_at_the_top() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.review.session.add_comment(
        Anchor {
            file: "src/lib.rs".to_owned(),
            line: Some(99),
            line_end: None,
            on_old_side: false,
            line_text: None,
        },
        "reviewer",
        "moved on",
    );
    let diff = app.diff.as_mut().unwrap();
    diff.invalidate();
    diff.ensure_rows(&app.review);
    let rows = rows(&app);
    assert!(
        matches!(rows.first(), Some(DiffRow::Comment { outdated: true, .. })),
        "orphaned comment sits at the top, flagged outdated: {rows:?}"
    );
}

#[test]
fn visual_selection_comments_a_new_side_range() {
    let fixture = two_hunk_fixture();
    let mut app = diff_app(&fixture);
    enter_diff_pane(&mut app);
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
    enter_diff_pane(&mut app);
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
    enter_diff_pane(&mut app);
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
    select_file(&mut app, "src/lib.rs");
    let position = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = position;
    app.handle(key('c'));
    type_text(&mut app, "question");
    app.handle(key('\n'));

    // the comment header row sits right under the anchored line
    settle_submit(&mut app);
    let position = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = position + 1;
    app.handle(key('r'));
    assert!(app.composer_open());
    type_text(&mut app, "answer");
    app.handle(key('\n'));
    let comment = &app.review.session.comments[0];
    assert_eq!(comment.status, CommentStatus::Replied);
    assert_eq!(comment.replies.len(), 1);
    assert_eq!(comment.replies[0].body, "answer");

    // the block grew by the reply line; resolve from the same header
    settle_submit(&mut app);
    let position = added_line_position(&app);
    app.diff.as_mut().unwrap().cursor = position + 1;
    app.handle(key('R'));
    assert_eq!(
        app.review.session.comments[0].status,
        CommentStatus::Resolved
    );
    let reloaded = diffler_core::store::load(&fixture.root).unwrap();
    assert_eq!(reloaded.comments[0].status, CommentStatus::Resolved);
}

#[test]
fn comment_display_lines_cover_body_and_replies() {
    let mut session = Session::default();
    let id = session
        .add_comment(
            Anchor {
                file: "a.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "first\nsecond",
        )
        .id
        .clone();
    session.reply(&id, "agent", "done\nand verified");
    let lines = comment_display(&session.comments[0], u16::MAX, None, None, None, false);
    let plain = |s: &str| MdSpan {
        text: s.to_owned(),
        ..MdSpan::default()
    };
    assert_eq!(
        lines,
        vec![
            CommentLine::Header,
            CommentLine::Body(vec![plain("first")]),
            CommentLine::Body(vec![plain("second")]),
            CommentLine::ReplyGap,
            CommentLine::ReplyHead {
                author: "agent".to_owned(),
                remote: false,
            },
            CommentLine::Reply {
                author: "agent".to_owned(),
                spans: vec![plain("done")],
            },
            CommentLine::Reply {
                author: "agent".to_owned(),
                spans: vec![plain("and verified")],
            },
            CommentLine::Footer,
        ]
    );
}

#[test]
fn comment_display_wraps_long_text_to_the_pane_width() {
    let mut session = Session::default();
    let id = session
        .add_comment(
            Anchor {
                file: "a.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "a body long enough that it cannot fit on one row",
        )
        .id
        .clone();
    session.reply(&id, "agent", "a reply that also runs past the pane");
    let lines = comment_display(&session.comments[0], 30, None, None, None, false);
    let budget = 30 - 4;
    let text = |runs: &[MdSpan]| runs.iter().map(|s| s.text.clone()).collect::<String>();
    for line in &lines {
        match line {
            CommentLine::Body(runs) => assert!(text(runs).width() <= budget, "{runs:?}"),
            CommentLine::Reply { spans, .. } => {
                let lane = crate::app::REPLY_LANE;
                assert!(text(spans).width() + lane <= budget, "{spans:?}");
            }
            _ => {}
        }
    }
    let bodies = lines
        .iter()
        .filter(|l| matches!(l, CommentLine::Body(_)))
        .count();
    assert!(bodies > 1, "the long body wrapped onto several rows");
    // words survive the wrap intact
    let joined: Vec<String> = lines
        .iter()
        .filter_map(|l| match l {
            CommentLine::Body(runs) => Some(text(runs)),
            _ => None,
        })
        .collect();
    assert_eq!(
        joined.join(" "),
        "a body long enough that it cannot fit on one row"
    );
}

fn thread_with(replies: &[(&str, &str)]) -> Session {
    let mut session = Session::default();
    let id = session
        .add_comment(
            Anchor {
                file: "a.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "why?",
        )
        .id
        .clone();
    for (author, body) in replies {
        session.reply(&id, author, body);
    }
    session
}

#[test]
fn a_closed_thread_folds_the_replies_before_the_latest() {
    let session = thread_with(&[("agent", "one"), ("reviewer", "two"), ("agent", "three")]);
    let closed = comment_display(&session.comments[0], u16::MAX, None, None, None, false);
    assert!(closed.contains(&CommentLine::FoldedReplies {
        count: 2,
        authors: vec!["agent".to_owned(), "reviewer".to_owned()],
    }));
    let heads = |lines: &[CommentLine]| {
        lines
            .iter()
            .filter(|line| matches!(line, CommentLine::ReplyHead { .. }))
            .count()
    };
    assert_eq!(heads(&closed), 1, "only the latest reply shows");
    let open = comment_display(&session.comments[0], u16::MAX, None, None, None, true);
    assert_eq!(heads(&open), 3, "an open thread shows every reply");
    assert!(
        !open
            .iter()
            .any(|line| matches!(line, CommentLine::FoldedReplies { .. }))
    );
}

#[test]
fn a_closed_thread_shows_its_latest_reply_in_full() {
    let session = thread_with(&[
        ("agent", "one"),
        ("reviewer", "two"),
        ("agent", "a\n\nb\n\nc\n\nd\n\ne"),
    ]);
    let closed = comment_display(&session.comments[0], u16::MAX, None, None, None, false);
    let body = closed
        .iter()
        .filter(|line| matches!(line, CommentLine::Reply { .. }))
        .count();
    assert!(
        body >= 5,
        "every line of the latest reply shows: {closed:?}"
    );
}

#[test]
fn tab_on_a_card_opens_and_closes_its_thread() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('c'));
    type_text(&mut app, "why 42?");
    app.handle(key('\n'));
    let id = app.review.session.comments[0].id.clone();
    for body in ["one", "two", "three"] {
        app.review.session.reply(&id, "agent", body);
    }
    let diff = app.diff.as_mut().unwrap();
    diff.mark_rows_dirty();
    diff.ensure_rows(&app.review);
    let card = rows(&app)
        .iter()
        .position(|r| matches!(r, DiffRow::Comment { .. }))
        .expect("the card");
    let diff = app.diff.as_mut().unwrap();
    diff.cursor = card;
    diff.focus = Pane::Diff;
    app.handle(AppEvent::Key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Tab,
    )));
    assert!(app.diff.as_ref().unwrap().open_threads.contains(&id));
    assert!(matches!(
        rows(&app)[app.diff.as_ref().unwrap().cursor],
        DiffRow::Comment { .. }
    ));
    app.handle(AppEvent::Key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Tab,
    )));
    assert!(!app.diff.as_ref().unwrap().open_threads.contains(&id));
}

#[test]
fn comment_display_hard_splits_an_unbreakable_token() {
    let mut session = Session::default();
    session.add_comment(
        Anchor {
            file: "a.rs".to_owned(),
            line: Some(1),
            line_end: None,
            on_old_side: false,
            line_text: None,
        },
        "reviewer",
        "https://example.invalid/a/very/long/unbroken/path/segment/thing",
    );
    let lines = comment_display(&session.comments[0], 24, None, None, None, false);
    for line in &lines {
        if let CommentLine::Body(runs) = line {
            let width: usize = runs.iter().map(|s| s.text.width()).sum();
            assert!(width <= 20, "{runs:?}");
        }
    }
}

#[test]
fn deleting_a_comment_asks_for_confirmation_first() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);
    app.handle(key('c'));
    type_text(&mut app, "why 42?");
    app.handle(key('\n'));

    let diff = app.diff.as_mut().unwrap();
    diff.ensure_rows(&app.review);
    let comment_row = rows(&app)
        .iter()
        .position(|r| matches!(r, DiffRow::Comment { .. }))
        .expect("comment row present");
    app.diff.as_mut().unwrap().cursor = comment_row;

    app.delete_comment_at_cursor();
    assert!(
        matches!(app.modal, Some(Modal::Confirm { .. })),
        "delete asks first"
    );
    assert_eq!(
        app.review.session.comments.len(),
        1,
        "nothing deleted before confirming"
    );
    app.confirm_modal();
    assert_eq!(
        app.review.session.comments.len(),
        0,
        "confirm deletes the comment"
    );
}

#[test]
fn claiming_a_comment_toggles_authorship_and_back() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    let crate::mcp::McpResponse::Added { .. } =
        app.handle_mcp(crate::mcp::McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "looks off".to_owned(),
            as_human: false,
        })
    else {
        panic!("expected an added comment");
    };
    select_file(&mut app, "src/lib.rs");
    let diff = app.diff.as_mut().unwrap();
    diff.ensure_rows(&app.review);
    let comment_row = rows(&app)
        .iter()
        .position(|r| matches!(r, DiffRow::Comment { .. }))
        .expect("comment row present");
    app.diff.as_mut().unwrap().cursor = comment_row;

    app.claim_comment_at_cursor();
    assert_eq!(app.review.session.comments[0].author, "reviewer");

    app.claim_comment_at_cursor();
    assert_eq!(
        app.review.session.comments[0].author,
        crate::mcp::AGENT_AUTHOR
    );
}

#[test]
fn claiming_on_a_row_that_is_not_a_comment_says_so() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    select_file(&mut app, "src/lib.rs");
    app.diff.as_mut().unwrap().cursor = added_line_position(&app);

    app.claim_comment_at_cursor();
    let message = app.message.clone().expect("info message");
    assert!(message.text.contains("move onto a comment"), "{message:?}");
}

#[test]
fn claiming_all_comments_asks_first_claims_every_agent_comment_and_leaves_a_human_one() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    let crate::mcp::McpResponse::Added { id: agent_id } =
        app.handle_mcp(crate::mcp::McpRequestKind::AddComment {
            file: "src/lib.rs".to_owned(),
            line: 2,
            line_end: None,
            body: "agent found this".to_owned(),
            as_human: false,
        })
    else {
        panic!("expected an added comment");
    };
    let human_id = app
        .review
        .session
        .add_comment(
            Anchor {
                file: "src/lib.rs".to_owned(),
                line: None,
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            "human's own",
        )
        .id
        .clone();

    app.claim_all_comments_start();
    assert!(
        matches!(app.modal, Some(Modal::Confirm { .. })),
        "claiming all asks first"
    );
    assert_eq!(
        app.review.session.comment(&agent_id).unwrap().author,
        crate::mcp::AGENT_AUTHOR,
        "nothing claimed before confirming"
    );

    app.handle(key('n'));
    assert!(app.modal.is_none(), "declining closes the dialog");
    assert_eq!(
        app.review.session.comment(&agent_id).unwrap().author,
        crate::mcp::AGENT_AUTHOR,
        "declining changes nothing"
    );

    app.claim_all_comments_start();
    app.handle(key('y'));
    assert_eq!(
        app.review.session.comment(&agent_id).unwrap().author,
        "reviewer",
        "confirming claims the agent comment"
    );
    assert_eq!(
        app.review.session.comment(&human_id).unwrap().author,
        "reviewer",
        "the human comment was always theirs, untouched"
    );
}

/// A walkthrough source refuses the verb: claiming a stop would hide it
/// from the revision logic that prunes superseded stops by agent
/// authorship.
#[test]
fn claiming_a_stop_on_a_walkthrough_is_refused() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    app.handle(key('j'));
    app.handle(key('j'));
    enter_diff_pane(&mut app);
    let card = rows(&app)
        .iter()
        .position(|row| matches!(row, DiffRow::Comment { line: 0, .. }))
        .expect("a stop card");
    app.diff.as_mut().expect("diff view").cursor = card;

    app.claim_comment_at_cursor();

    let message = app.message.clone().expect("info message");
    assert!(message.text.contains("claimable"), "{message:?}");
    let source = app.active_review_source();
    assert_eq!(
        app.review
            .session_for(&source)
            .comment("stop-2")
            .unwrap()
            .author,
        crate::mcp::AGENT_AUTHOR,
        "the stop stays the agent's"
    );
}

#[test]
fn claiming_all_comments_on_a_walkthrough_is_refused_without_a_prompt() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);

    app.claim_all_comments_start();

    assert!(app.modal.is_none(), "no confirm prompt for a walkthrough");
    let message = app.message.clone().expect("info message");
    assert!(message.text.contains("claimable"), "{message:?}");
    let source = app.active_review_source();
    assert!(
        app.review
            .session_for(&source)
            .comments
            .iter()
            .all(|c| c.author == crate::mcp::AGENT_AUTHOR),
        "every stop stays the agent's"
    );
}
