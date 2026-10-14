//! Tests for the walkthrough layout, its slides and stop cards.

use super::*;

/// The stop list replaces the file list: one row per stop, in the
/// agent's order, with no group headers between them. A walkthrough with
/// no summary has no leading row at all.
#[test]
fn a_walkthrough_with_no_summary_lists_only_its_stops() {
    let fixture = standard_fixture();
    let app = walkthrough_app(&fixture);
    assert_eq!(
        tree_kinds(&app),
        vec![
            "stop:0".to_owned(),
            "stop:1".to_owned(),
            "stop:2".to_owned()
        ]
    );
}

/// A walkthrough with a summary lists it as the leading row.
#[test]
fn a_walkthrough_with_a_summary_lists_it_as_the_leading_row() {
    let fixture = standard_fixture();
    let app = walkthrough_app_with_summary(&fixture);
    assert_eq!(
        tree_kinds(&app),
        vec![
            "walkthrough_summary".to_owned(),
            "stop:0".to_owned(),
            "stop:1".to_owned(),
            "stop:2".to_owned()
        ]
    );
}

/// Selecting the leading row shows the summary slide.
#[test]
fn the_leading_summary_row_selects_the_summary_slide() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app_with_summary(&fixture);
    assert_eq!(
        tree_cursor(&app),
        1,
        "starts on the first stop, one past the summary"
    );

    app.diff_tree_to(0);

    assert_eq!(
        app.diff.as_ref().expect("diff view").slide,
        Some(Slide::Summary)
    );
    assert_eq!(tree_cursor(&app), 0);
}

/// The summary slide renders one card and no diff rows at all, the way a
/// stop with no anchored line already shows its own card alone.
#[test]
fn the_summary_slide_shows_its_card_and_no_diff_rows() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app_with_summary(&fixture);

    app.diff_tree_to(0);

    assert!(
        rows(&app)
            .iter()
            .all(|row| matches!(row, DiffRow::Summary { .. })),
        "{:?}",
        rows(&app)
    );
    assert!(!rows(&app).is_empty());
}

/// A `mermaid` fence in the summary renders as a figure through the same
/// path a comment's body does.
#[test]
fn a_mermaid_fence_in_the_summary_becomes_a_figure() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    crate::test_support::set_walkthrough_summary(
        &mut app,
        "w1",
        "why\n\n```mermaid\nflowchart LR\n  a --> b\n```\n",
    );
    app.diff.as_mut().unwrap().invalidate();
    app.seat_summary();

    let diff = app.diff.as_ref().expect("diff view");
    let key = crate::app::walkthrough::summary_figure_key("w1");
    assert!(
        diff.figures.get(&key).is_some_and(|cached| cached
            .blocks
            .iter()
            .any(|block| matches!(block, crate::app::walkthrough::Block::Figure(_)))),
        "the fence parsed into a figure block"
    );
}

const SUMMARY_WITH_FLOWCHART: &str = r"Under Rosetta 2, `vcmpss` with the NEQ, NLT or NLE predicate writes its mask into a register, and if that register is then reloaded by a full width VEX load, later scalar reads of it still return the old mask. Games then compute NaN from finite inputs (in DOAX3 a bone slerp writes the invalid matrix marker and whole body parts vanish). This PR adds a macOS entry to the existing `Patches` table that upstream's ahead-of-time static patcher applies, and extends the patcher's gap sweep so compares outside EH frame functions are patched too.

```mermaid
flowchart TD
  A[module.cpp static patching on macOS] --> B[PatchSegmentStatically: EH frame functions]
  A --> C[PatchUncoveredCpuInstructions: gaps between them]
  B --> D[FindMatchingPatch / TryPatch over Patches]
  C -->|FindScalarCompareInstructionStart| D
  D -->|VCMPSS + FilterNegatedScalarCompare| E[GenerateNegatedScalarCompare trampoline]
  E --> F[vcmpss with plain predicate, then vxorps lane 0]
```";

/// The summary's flowchart opens full screen like a stop's: the reader
/// walks onto it in the pane and presses `o`, even after the rows were
/// rebuilt under them.
#[test]
fn the_summarys_flowchart_opens_full_screen() {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(145, 43)).expect("terminal");
    let mut draw = |app: &mut App| {
        terminal
            .draw(|frame| crate::ui::draw(frame, app))
            .expect("draw");
    };
    let fixture = standard_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    let crate::mcp::McpResponse::WalkthroughPublished(published) =
        app.handle_mcp(crate::mcp::McpRequestKind::PublishWalkthrough {
            id: None,
            title: "flow".to_owned(),
            stops: vec![crate::mcp::StopParams {
                id: None,
                title: "The answer".to_owned(),
                anchor: Some("src/lib.rs#answer".to_owned()),
                body: "why".to_owned(),
                notes: None,
            }],
            skipped: None,
            summary: Some(SUMMARY_WITH_FLOWCHART.to_owned()),
        })
    else {
        panic!("expected a published walkthrough");
    };
    app.open_walkthrough(&published.id, Slide::Summary);
    draw(&mut app);
    let request = app.pending_walkthrough.take().expect("an anchor read");
    let read = diffler_core::review::Review::compute_walkthrough_files(
        &app.review.repo_root,
        request.read_rev.as_deref(),
        request.read_first,
        &request.files,
    );
    app.handle(AppEvent::WalkthroughAnchors {
        contents: read.contents,
        pin_broken: read.pin_broken,
        token: request.token,
    });
    draw(&mut app);
    app.handle(key('l'));
    app.handle(key('G'));
    for _ in 0..6 {
        app.handle(key('k'));
    }
    // a background result (highlighting, anchors) rebuilds the rows
    // under the reader, which must leave the cursor where it was
    let diff = app.diff.as_mut().expect("diff");
    diff.mark_rows_dirty();
    diff.ensure_rows(&app.review);
    app.handle(key('o'));
    assert_eq!(app.screen(), Screen::Graph, "{:?}", app.message);
}

/// `display_order` drives the file walks, so it has to be the stops' files
/// in stop order with the repeats dropped.
#[test]
fn the_walkthrough_layout_orders_files_by_their_stops() {
    let fixture = standard_fixture();
    let app = walkthrough_app(&fixture);
    let diff = app.diff.as_ref().expect("diff view");
    let model = diff.model(&app.review);
    let session = app.review.session_for(&diff.source);
    let paths: Vec<&str> = diff
        .display_order(model, session)
        .into_iter()
        .filter_map(|index| model.files.get(index))
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["todo.md", "src/lib.rs"]);
}

/// `t` reaches the walkthrough layout only on a walkthrough's own source,
/// even when a walkthrough exists elsewhere.
#[test]
fn t_cycles_into_the_walkthrough_only_on_its_own_source() {
    let fixture = standard_fixture();
    let mut app = diff_app(&fixture);
    seat_walkthrough(&mut app);
    for _ in 0..3 {
        app.handle(key('t'));
    }
    assert_eq!(
        layout_of(&app),
        crate::config::FileLayout::Tree,
        "a walkthrough elsewhere never enters this source's cycle"
    );

    // `open_walkthrough` seats the layout on Walkthrough directly, so the
    // cycle from here runs Tree, Review, Kinds, back to Walkthrough
    app.open_walkthrough("w1", Slide::Stop(0));
    let layouts: Vec<crate::config::FileLayout> = (0..4)
        .map(|_| {
            app.handle(key('t'));
            layout_of(&app)
        })
        .collect();
    assert_eq!(
        layouts,
        [
            crate::config::FileLayout::Tree,
            crate::config::FileLayout::Review,
            crate::config::FileLayout::Kinds,
            crate::config::FileLayout::Walkthrough,
        ]
    );
}

/// A clean tree gives tree/review/kinds nothing to list, so `t` cycles
/// back to the walkthrough.
#[test]
fn t_stays_on_the_walkthrough_when_the_diff_has_no_files_of_its_own() {
    let fixture = Fixture::new();
    fixture.write("README.md", "hello\n");
    fixture.commit_all("initial commit");
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    crate::test_support::seat_walkthrough(
        &mut app,
        "tour",
        &[("Base", Some("README.md:1"), "why hello")],
    );
    app.open_walkthrough("w1", Slide::Stop(0));
    assert_eq!(layout_of(&app), crate::config::FileLayout::Walkthrough);

    app.handle(key('t'));

    assert_eq!(
        layout_of(&app),
        crate::config::FileLayout::Walkthrough,
        "tree/review/kinds would list nothing; t stays put"
    );
}

fn layout_of(app: &App) -> crate::config::FileLayout {
    app.diff.as_ref().expect("diff view").layout
}

/// Cycling into the walkthrough must show its stop cards immediately, and
/// cycling back out must drop them and any span band with them.
#[test]
fn cycling_into_and_out_of_the_walkthrough_toggles_its_stop_cards() {
    let fixture = standard_fixture();
    let mut app = App::new(fixture.review(), LoadedConfig::default());
    app.author = "reviewer".to_owned();
    seat_walkthrough(&mut app);
    app.open_walkthrough_diff("w1");
    resolve_walkthrough(&mut app);
    assert_eq!(layout_of(&app), crate::config::FileLayout::Tree);

    for _ in 0..3 {
        app.handle(key('t'));
    }
    assert_eq!(layout_of(&app), crate::config::FileLayout::Walkthrough);
    app.diff.as_mut().unwrap().ensure_rows(&app.review);
    assert!(
        stop_card_rows(&app) > 0,
        "the walkthrough shows its stop card once cycled in"
    );
    let windowed = line_rows(&app);
    assert!(
        shown_line_numbers(&app).iter().all(|no| *no <= 3),
        "and only the current stop's own span"
    );

    app.handle(key('t'));
    assert_eq!(layout_of(&app), crate::config::FileLayout::Tree);
    app.diff.as_mut().unwrap().ensure_rows(&app.review);
    assert!(
        line_rows(&app) > windowed,
        "leaving the walkthrough shows the whole file again"
    );
    assert!(
        app.diff.as_ref().expect("diff view").referenced.is_none(),
        "leaving the walkthrough drops the span band too"
    );
}

/// Diff-line rows on screen, deletions included.
fn line_rows(app: &App) -> usize {
    rows(app)
        .iter()
        .filter(|row| matches!(row, DiffRow::Line { .. }))
        .count()
}

/// Rows of the current stop's own card on screen. Row 0 of the sidebar is
/// the leading all-slides row, so a stop's own row sits one past it.
fn stop_card_rows(app: &App) -> usize {
    let diff = app.diff.as_ref().expect("diff view");
    let session = app.review.session_for(&diff.source);
    let Some(walkthrough) = diff.active_walkthrough(session) else {
        return 0;
    };
    let offset = super::stop_row_offset(walkthrough);
    let Some(current) = diff
        .tree_cursor
        .checked_sub(offset)
        .and_then(|stop| walkthrough.stops.get(stop))
        .and_then(|id| session.comments.iter().position(|c| c.id == *id))
    else {
        return 0;
    };
    rows(app)
        .iter()
        .filter(|row| matches!(row, DiffRow::Comment { comment, .. } if *comment == current))
        .count()
}

/// A stop seats the reader on the first row of the span the agent named,
/// in the file that holds it, and shows that span.
#[test]
fn moving_onto_a_stop_seats_the_cursor_on_its_span() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    // stop 2 anchors src/lib.rs#answer, whose definition is lines 1..=3
    app.handle(key('j'));
    app.handle(key('j'));
    assert_eq!(selected_path(&app), "src/lib.rs");
    let diff = app.diff.as_ref().expect("diff view");
    assert_eq!(
        diff.cursor, 1,
        "the cursor lands on the first row of the span"
    );
    let last = diff.rows().len().saturating_sub(1);
    let lines: Vec<u32> = (diff.cursor..=last)
        .filter_map(|row| match diff.rows().get(row) {
            Some(DiffRow::Line { hunk, line, .. }) => diff
                .model(&app.review)
                .files
                .get(diff.selected)
                .and_then(|file| file.hunks.get(*hunk))
                .and_then(|hunk| hunk.lines.get(*line))
                .and_then(|line| line.new_no),
            _ => None,
        })
        .collect();
    assert_eq!(lines, [1, 2, 3], "the slide shows the whole definition");
}

/// A stop with no anchor leaves the pane where the reader had it.
#[test]
fn an_anchorless_stop_leaves_the_pane_alone() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    app.handle(key('j'));
    let seated = selected_path(&app);
    assert_eq!(seated, "todo.md");
    app.handle(key('k'));
    assert_eq!(selected_path(&app), seated, "stop 1 anchors nothing");
    assert_eq!(
        app.diff.as_ref().expect("diff view").referenced,
        None,
        "and bands nothing"
    );
}

/// A stop is a comment, so `r` on its card answers it in its own thread.
#[test]
fn r_on_a_stop_card_replies_to_that_stop() {
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
    app.handle(key('r'));
    assert!(app.composer_open());
    type_text(&mut app, "why not 43?");
    app.handle(key('\n'));

    let source = app.active_review_source();
    let stop = app
        .review
        .session_for(&source)
        .comment("stop-2")
        .expect("the third stop's comment");
    assert_eq!(stop.replies.len(), 1);
    assert_eq!(stop.replies[0].body, "why not 43?");
    assert_eq!(stop.status, CommentStatus::Replied);
}

/// The stops are comments, so the pane that lists comments lists them
/// with no handling of its own.
#[test]
fn the_comments_pane_lists_the_stops() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    app.handle(key('C'));
    assert_eq!(
        app.comment_order(),
        vec![
            "stop-2".to_owned(),
            "stop-0".to_owned(),
            "stop-1".to_owned()
        ],
        "by file as the diff orders them, then by line"
    );
}

/// A comment the reader left, on a line of the open review.
fn add_comment(app: &mut App, file: &str, line: u32, body: &str) -> String {
    let source = app.active_review_source();
    let id = app
        .review
        .session_for_mut(&source)
        .add_comment(
            Anchor {
                file: file.to_owned(),
                line: Some(line),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            "reviewer",
            body,
        )
        .id
        .clone();
    let diff = app.diff.as_mut().expect("diff view");
    diff.invalidate();
    diff.ensure_rows(&app.review);
    id
}

/// Comment ids the pane currently shows a card for.
fn shown_comment_ids(app: &App) -> Vec<String> {
    let diff = app.diff.as_ref().expect("diff view");
    let session = app.review.session_for(&diff.source);
    rows(app)
        .iter()
        .filter_map(|row| match row {
            DiffRow::Comment {
                comment, line: 0, ..
            } => session.comments.get(*comment).map(|c| c.id.clone()),
            _ => None,
        })
        .collect()
}

/// Select the comment `id` in the open comments sidebar, the way a motion
/// key in that pane does.
fn select_comment(app: &mut App, id: &str) {
    let at = app
        .comment_order()
        .iter()
        .position(|known| known == id)
        .expect("the comment is listed");
    app.comments_to(at);
}

/// Selecting a comment in the comments pane enters the slide that holds it.
#[test]
fn selecting_a_comment_from_the_pane_enters_the_slide_that_holds_it() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    let human = add_comment(&mut app, "src/lib.rs", 2, "why 42?");
    assert_eq!(tree_cursor(&app), 0, "the reader starts on the first stop");

    app.handle(key('C'));
    select_comment(&mut app, &human);

    assert_eq!(tree_cursor(&app), 2, "the sidebar follows to that stop");
    assert_eq!(selected_path(&app), "src/lib.rs");
    assert_eq!(shown_line_numbers(&app), [1, 2, 3], "the stop's own region");
    assert_eq!(
        shown_comment_ids(&app),
        vec![human, "stop-2".to_owned()],
        "cards in line order: line 2's comment before the stop's own card, \
         which sits under line 3, the last line of its span"
    );
}

/// A comment no stop's region holds is a slide of its own.
#[test]
fn a_comment_outside_every_region_opens_as_its_own_slide() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    let human = add_comment(&mut app, "ci.yml", 1, "why on push?");

    app.handle(key('C'));
    select_comment(&mut app, &human);

    let diff = app.diff.as_ref().expect("diff view");
    assert_eq!(diff.slide, Some(Slide::AdHoc(human.clone())));
    assert_eq!(selected_path(&app), "ci.yml");
    assert_eq!(shown_line_numbers(&app), [1]);
    assert_eq!(shown_comment_ids(&app), vec![human]);
}

/// A comment on a deleted line anchors old-side, so reaching it through
/// the comments pane windows the deleted row and its hunk header in along
/// with the card.
#[test]
fn a_comment_on_a_deleted_line_bands_the_deleted_row_and_its_hunk() {
    let fixture = two_hunk_fixture();
    let mut app = walkthrough_app_with(
        &fixture,
        &[("Somewhere else", None, "unrelated to the deletion")],
    );
    let source = app.active_review_source();
    let human = app
        .review
        .session_for_mut(&source)
        .add_comment(
            Anchor {
                file: "data.txt".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: true,
                line_text: None,
            },
            "reviewer",
            "why drop this line?",
        )
        .id
        .clone();
    app.diff.as_mut().expect("diff view").invalidate();
    app.diff
        .as_mut()
        .expect("diff view")
        .ensure_rows(&app.review);

    app.handle(key('C'));
    select_comment(&mut app, &human);

    let diff = app.diff.as_ref().expect("diff view");
    assert_eq!(diff.slide, Some(Slide::AdHoc(human.clone())));
    let shown = rows(&app);
    assert!(
        shown
            .iter()
            .any(|row| matches!(row, DiffRow::Hunk { hunk: 0, .. })),
        "the deleted row's hunk header must be windowed in: {shown:?}"
    );
    assert!(
        shown.iter().any(|row| matches!(
            row,
            DiffRow::Line {
                hunk: 0,
                line: 0,
                ..
            }
        )),
        "the deleted row itself must be windowed in, not only the card: {shown:?}"
    );
}

/// The comment walk runs over the slides, so stepping past the last
/// comment of one enters the next.
#[test]
fn stepping_past_a_slides_last_comment_enters_the_next_slide() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    let human = add_comment(&mut app, "todo.md", 1, "is this list still right?");
    app.handle(key('C'));
    select_comment(&mut app, &human);
    assert_eq!(tree_cursor(&app), 1, "the second stop holds it");
    assert_eq!(
        shown_comment_ids(&app),
        vec!["stop-1".to_owned(), human],
        "that slide holds both"
    );

    app.handle(key('}'));

    assert_eq!(tree_cursor(&app), 2, "the walk moved on to the next stop");
    assert_eq!(selected_path(&app), "src/lib.rs");
    assert_eq!(shown_line_numbers(&app), [1, 2, 3]);
    assert_eq!(shown_comment_ids(&app), vec!["stop-2".to_owned()]);
}

/// New-side line numbers of every `DiffRow::Line` the pane currently
/// shows, wherever the file lives in the model.
fn shown_line_numbers(app: &App) -> Vec<u32> {
    let diff = app.diff.as_ref().expect("diff view");
    let model = diff.model(&app.review);
    rows(app)
        .iter()
        .filter_map(|row| {
            let DiffRow::Line { file, hunk, line } = row else {
                return None;
            };
            model
                .files
                .get(*file)?
                .hunks
                .get(*hunk)?
                .lines
                .get(*line)?
                .new_no
        })
        .collect()
}

/// A stop anchored deep in a large file windows the pane to exactly the
/// span the agent pointed at, its hunk header and its card.
#[test]
fn a_stops_window_is_its_anchored_rows_its_hunk_header_and_its_card() {
    let fixture = big_file_fixture();
    let app = walkthrough_app_with(
        &fixture,
        &[("The change", Some("big.txt:100"), "why line 100 changed")],
    );
    assert_eq!(
        shown_line_numbers(&app),
        [100],
        "the anchored line and no context around it"
    );
    assert!(
        rows(&app)
            .iter()
            .any(|row| matches!(row, DiffRow::Hunk { .. })),
        "the hunk header it sits under stays"
    );
    assert!(stop_card_rows(&app) > 0, "the card still draws");
}

/// The window shows a wide span whole, with no row budget.
#[test]
fn a_wide_span_shows_every_row_it_covers() {
    let fixture = huge_span_fixture();
    let app = walkthrough_app_with(
        &fixture,
        &[("The rewrite", Some("big.txt:51-120"), "why 70 lines moved")],
    );
    assert_eq!(shown_line_numbers(&app).len(), 70);
}

/// A stop whose anchor names a file but resolves to no span (here, a bare
/// path) shows only its card.
#[test]
fn an_anchor_with_no_span_shows_only_its_card() {
    let fixture = standard_fixture();
    let app = walkthrough_app_with(
        &fixture,
        &[("Whole file", Some("todo.md"), "look at the whole file")],
    );
    assert!(
        rows(&app)
            .iter()
            .all(|row| matches!(row, DiffRow::Comment { .. })),
        "no hunks at all: {:?}",
        rows(&app)
    );
}

/// Cycling `t` out of the walkthrough drops the window: the file reads
/// whole again, the way any other layout shows it.
#[test]
fn cycling_out_of_the_walkthrough_restores_the_full_file_rows() {
    let fixture = huge_span_fixture();
    let mut app = walkthrough_app_with(
        &fixture,
        &[("The rewrite", Some("big.txt:51-120"), "why 70 lines moved")],
    );
    let windowed = shown_line_numbers(&app).len();
    app.handle(key('t'));
    app.diff.as_mut().unwrap().ensure_rows(&app.review);
    assert_eq!(
        layout_of(&app),
        crate::config::FileLayout::Tree,
        "left the walkthrough"
    );
    assert!(
        shown_line_numbers(&app).len() > windowed,
        "the whole hunk shows again, not just the window"
    );
}

/// `<c-n>` means "the next thing the sidebar lists", which in this layout
/// is the next stop.
#[test]
fn ctrl_n_steps_stops_in_the_walkthrough_layout() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    app.handle(ctrl_key('n'));
    assert_eq!(tree_cursor(&app), 1);
    assert_eq!(selected_path(&app), "todo.md");
    app.handle(ctrl_key('n'));
    assert_eq!(tree_cursor(&app), 2);
    assert_eq!(selected_path(&app), "src/lib.rs");
    app.handle(ctrl_key('p'));
    assert_eq!(tree_cursor(&app), 1);
}

/// `m` in the walkthrough layout marks the slide read and moves on, the
/// way `m` on a file advances to the row below it.
#[test]
fn m_marks_the_current_slide_seen_and_advances_to_the_next() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    assert_eq!(app.diff.as_ref().unwrap().slide, Some(Slide::Stop(0)));

    app.handle(key('m'));

    let source = app.active_review_source();
    assert!(app.review.session_for(&source).is_stop_seen("stop-0"));
    assert_eq!(app.diff.as_ref().unwrap().slide, Some(Slide::Stop(1)));
}

/// `u` skips a slide already marked seen and lands on the next one that
/// isn't.
#[test]
fn u_jumps_to_the_next_unseen_slide_skipping_seen_ones() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    let source = app.active_review_source();
    app.review.session_for_mut(&source).mark_stop_seen("stop-1");

    app.handle(key('u'));

    assert_eq!(
        app.diff.as_ref().unwrap().slide,
        Some(Slide::Stop(2)),
        "stop-1 is seen, so u skips past it to stop-2"
    );
}

#[test]
fn u_says_every_slide_is_seen_when_none_is_left() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    let source = app.active_review_source();
    for id in ["stop-0", "stop-1", "stop-2"] {
        app.review.session_for_mut(&source).mark_stop_seen(id);
    }

    app.handle(key('u'));

    assert_eq!(
        app.message.as_ref().map(|m| m.text.as_str()),
        Some("every slide is seen")
    );
}

/// The diff sidebar's own heading counts the walkthrough's stops seen so
/// far.
#[test]
fn seen_progress_shows_in_the_sidebar_heading_not_the_status_header() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    let source = app.active_review_source();
    app.review.session_for_mut(&source).mark_stop_seen("stop-0");

    let content = crate::test_support::render(&mut app).backend().to_string();
    assert!(
        content.contains("How the answer moved (1/3)"),
        "sidebar heading: {content}"
    );

    app.handle(key('q'));
    let content = crate::test_support::render(&mut app).backend().to_string();
    assert!(
        content.contains("Walkthroughs (1)"),
        "status header: {content}"
    );
}

/// `d` on the walkthrough layout's leading row asks, then deletes the
/// whole review file, closing the diff with it.
#[test]
fn d_on_the_leading_sidebar_row_asks_then_deletes_the_whole_walkthrough() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app_with_summary(&fixture);
    app.diff.as_mut().unwrap().tree_cursor = 0;

    app.handle(key('d'));
    assert!(matches!(app.modal, Some(crate::app::Modal::Confirm { .. })));
    app.handle(key('y'));

    assert!(app.diff.is_none(), "the diff closes with it");
    let source = diffler_core::source::ReviewSource::walkthrough("w1");
    assert!(
        app.review
            .all_reviews()
            .expect("all reviews")
            .into_iter()
            .all(|(s, _)| s != source),
        "the review file is gone"
    );
}

/// `d` on a stop row asks, then deletes just that stop, keeping the rest
/// of the walkthrough.
#[test]
fn d_on_a_stop_row_asks_then_deletes_just_that_stop() {
    let fixture = standard_fixture();
    let mut app = walkthrough_app(&fixture);
    // no summary, so row 0 is stop 0's own row
    app.diff.as_mut().unwrap().tree_cursor = 0;

    app.handle(key('d'));
    app.handle(key('y'));

    let source = app.active_review_source();
    let walkthrough = app
        .review
        .session_for(&source)
        .walkthrough
        .as_ref()
        .expect("the walkthrough is kept");
    assert_eq!(walkthrough.stops.len(), 2);
    assert!(!walkthrough.stops.contains(&"stop-0".to_owned()));
}
