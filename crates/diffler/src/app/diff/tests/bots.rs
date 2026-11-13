use super::*;

fn anchor(line: u32) -> Anchor {
    Anchor {
        file: "data.txt".to_owned(),
        line: Some(line),
        line_end: None,
        on_old_side: false,
        line_text: None,
        on_file: false,
    }
}

/// A Copilot thread on line 1, and a person's thread on line 20 with a bot reply.
fn app_with_bot_comments(fixture: &crate::test_support::Fixture) -> App {
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.review
        .session
        .add_comment(anchor(1), "Copilot[bot]", "consider a constant");
    let human = app
        .review
        .session
        .add_comment(anchor(20), "reviewer", "why this way?")
        .id
        .clone();
    app.review
        .session
        .reply(&human, "coderabbitai[bot]", "summary of changes");
    app.open_working_tree_diff(None);
    let diff = app.diff.as_mut().expect("diff view");
    diff.focus = Pane::Diff;
    diff.invalidate();
    app
}

fn shown_comments(app: &mut App) -> Vec<String> {
    let diff = app.diff.as_mut().expect("diff view");
    diff.ensure_rows(&app.review);
    let session = app.review.session_for(&diff.source);
    diff.rows()
        .iter()
        .filter_map(|row| match row {
            DiffRow::Comment {
                comment, line: 0, ..
            } => session.comments.get(*comment).map(|c| c.author.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn h_hides_bot_threads_and_bot_replies_and_counts_them() {
    let fixture = two_hunk_fixture();
    let mut app = app_with_bot_comments(&fixture);
    assert_eq!(shown_comments(&mut app), ["Copilot[bot]", "reviewer"]);
    assert_eq!(app.hidden_bot_count(), 0);

    app.handle(key('H'));
    assert!(app.hides_bots());
    assert_eq!(shown_comments(&mut app), ["reviewer"]);
    assert_eq!(
        app.comment_order().len(),
        1,
        "the comment list drops it too"
    );
    assert_eq!(
        app.hidden_bot_count(),
        2,
        "the bot thread and the bot reply"
    );
    let diff = app.diff.as_ref().expect("diff view");
    let session = app.review.session_for(&diff.source);
    let person = session.comments.get(1).expect("the person's thread");
    let lines = diff.card_views().lines(person, u16::MAX);
    assert!(
        !lines
            .iter()
            .any(|line| matches!(line, CommentLine::ReplyHead { .. })),
        "the bot reply leaves the person's thread"
    );

    app.handle(key('H'));
    assert_eq!(shown_comments(&mut app), ["Copilot[bot]", "reviewer"]);
    assert_eq!(app.hidden_bot_count(), 0);
}

#[test]
fn a_bot_thread_a_person_answered_stays_in_sight() {
    let fixture = two_hunk_fixture();
    let mut app = app_with_bot_comments(&fixture);
    let bot = app.review.session.comments[0].id.clone();
    app.review.session.reply(&bot, "reviewer", "good catch");
    app.handle(key('H'));
    assert_eq!(shown_comments(&mut app), ["Copilot[bot]", "reviewer"]);
}

#[test]
fn hide_bots_in_the_config_starts_hidden_and_counts_listed_accounts() {
    let fixture = two_hunk_fixture();
    let mut config = LoadedConfig::default();
    config.config.review.hide_bots = true;
    config.config.review.bots = vec!["release-helper".to_owned()];
    let mut app = App::new(fixture.review(), config);
    app.review
        .session
        .add_comment(anchor(1), "release-helper", "bumped the version");
    app.review
        .session
        .add_comment(anchor(20), "reviewer", "why this way?");
    app.open_working_tree_diff(None);
    assert_eq!(shown_comments(&mut app), ["reviewer"]);
}
