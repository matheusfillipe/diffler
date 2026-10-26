// helpers run outside #[test] fns, where clippy's test allowances don't reach
#![allow(clippy::expect_used)]

mod common;

use common::Fixture;
use diffler_core::review::Review;
use diffler_core::session::Anchor;

fn anchor(file: &str, line: u32) -> Anchor {
    Anchor {
        file: file.to_owned(),
        line: Some(line),
        line_end: None,
        on_old_side: false,
        line_text: None,
    }
}

#[test]
fn review_persists_session_and_auto_resets_viewed_on_rewrite() {
    let fx = Fixture::new();
    fx.write("a.py", "def f():\n    return 1\n");
    fx.commit_all("base");
    fx.write("a.py", "def f():\n    return 2\n");

    let mut review = Review::open(fx.root()).expect("open");
    assert_eq!(review.model().files.len(), 1);
    assert_eq!(review.model().files[0].path, "a.py");
    assert_eq!(review.status.unstaged.files.len(), 1);

    let hash = review.model().files[0].content_hash();
    review
        .session
        .add_comment(anchor("a.py", 2), "reviewer", "why 2?");
    review.session.mark_viewed("a.py", &hash);
    review.save().expect("save");

    let mut review = Review::open(fx.root()).expect("reopen");
    assert_eq!(review.session.comments.len(), 1);
    assert_eq!(review.session.comments[0].body, "why 2?");
    let current = review.model().files[0].content_hash();
    assert!(review.session.is_viewed("a.py", &current));

    fx.write("a.py", "def f():\n    return 3\n");
    review.refresh().expect("refresh");
    assert!(review.session.viewed.is_empty());
    let current = review.model().files[0].content_hash();
    assert!(!review.session.is_viewed("a.py", &current));
    assert_eq!(review.session.comments.len(), 1);
}

/// The new-side line number of `text` in `file`'s diff.
fn diff_line(review: &Review, file: &str, text: &str) -> Option<u32> {
    review
        .model()
        .files
        .iter()
        .find(|f| f.path == file)?
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .find(|line| line.text == text)?
        .new_no
}

/// A review of `before` (committed) changed to `after`, with a comment on the
/// changed line reading `commented`.
fn commented_review(fx: &Fixture, before: &str, after: &str, commented: &str) -> Review {
    fx.write("lib.rs", before);
    fx.commit_all("base");
    fx.write("lib.rs", after);
    let mut review = Review::open(fx.root()).expect("open");
    let line = diff_line(&review, "lib.rs", commented).expect("commented line in the diff");
    let mut anchor = anchor("lib.rs", line);
    anchor.line_text = Some(commented.to_owned());
    review.session.add_comment(anchor, "reviewer", "check this");
    review
}

/// Where the comment sits after a refresh, and whether it reads outdated.
fn after_edit(fx: &Fixture, review: &mut Review, edited: &str) -> (Option<u32>, bool) {
    fx.write("lib.rs", edited);
    review.refresh().expect("refresh");
    let anchor = &review.session.comments.first().expect("the comment").anchor;
    (anchor.line, anchor.is_outdated(review.model()))
}

const BEFORE: &str = "fn a() {\n    one();\n}\n\nfn b() {\n    two();\n}\n";
const AFTER: &str = "fn a() {\n    one();\n}\n\nfn b() {\n    three();\n}\n";

#[test]
fn a_comment_follows_its_line_when_lines_land_above_it() {
    let fx = Fixture::new();
    let mut review = commented_review(&fx, BEFORE, AFTER, "    three();");
    let edited = format!("use std::fmt;\nuse std::io;\n\n{AFTER}");
    assert_eq!(after_edit(&fx, &mut review, &edited), (Some(9), false));
}

#[test]
fn a_comment_follows_its_line_when_lines_above_it_go() {
    let fx = Fixture::new();
    let before = format!("use std::fmt;\nuse std::io;\n\n{BEFORE}");
    let after = format!("use std::fmt;\nuse std::io;\n\n{AFTER}");
    let mut review = commented_review(&fx, &before, &after, "    three();");
    assert_eq!(after_edit(&fx, &mut review, AFTER), (Some(6), false));
}

#[test]
fn a_comment_on_a_repeated_line_follows_its_own_copy() {
    let fx = Fixture::new();
    // the comment sits on fn b's closing brace, line 7; fn a closes with the
    // same text on line 3
    let after = "fn a() {\n    one();\n}\n\nfn b() {\n    three();\n}\n\nfn c() {}\n";
    let mut review = commented_review(&fx, BEFORE, after, "    three();");
    let anchor = &mut review.session.comments[0].anchor;
    anchor.line = Some(7);
    anchor.line_text = Some("}".to_owned());
    // four lines land above, so fn a's brace now sits on line 7 and fn b's on 11
    let edited = format!("use std::fmt;\nuse std::io;\nuse std::fs;\n\n{after}");
    assert_eq!(after_edit(&fx, &mut review, &edited), (Some(11), false));
}

#[test]
fn a_range_comment_moves_as_one_block() {
    let fx = Fixture::new();
    let mut review = commented_review(&fx, BEFORE, AFTER, "}");
    let anchor = &mut review.session.comments[0].anchor;
    // fn b from its signature to its closing brace
    anchor.line = Some(5);
    anchor.line_end = Some(7);
    let edited = format!("use std::fmt;\n\n{AFTER}");
    fx.write("lib.rs", &edited);
    review.refresh().expect("refresh");
    let anchor = &review.session.comments[0].anchor;
    assert_eq!((anchor.line, anchor.line_end), (Some(7), Some(9)));
    assert!(!anchor.is_outdated(review.model()));
}

#[test]
fn a_comment_whose_line_was_rewritten_stays_put_and_reads_outdated() {
    let fx = Fixture::new();
    // fn c repeats the commented line, so the comment has a twin to jump to
    let after = format!("{AFTER}\nfn c() {{\n    three();\n}}\n");
    let mut review = commented_review(&fx, BEFORE, &after, "    three();");
    let edited = after.replacen("three", "four", 1);
    assert_eq!(after_edit(&fx, &mut review, &edited), (Some(6), true));
}

#[test]
fn an_edit_next_to_a_repeated_line_leaves_the_comment_on_its_own_copy() {
    let fx = Fixture::new();
    let after = "fn a() {\n    x();\n}\n\nfn b() {\n    x();\n}\n";
    let mut review = commented_review(&fx, BEFORE, after, "    x();");
    let anchor = &mut review.session.comments[0].anchor;
    // fn b's closing brace, with fn a's identical brace four lines up
    anchor.line = Some(7);
    anchor.line_text = Some("}".to_owned());
    let edited = after.replace("fn b() {\n    x();", "fn b() {\n    y();");
    assert_eq!(after_edit(&fx, &mut review, &edited), (Some(7), false));
}

#[test]
fn an_edit_right_above_a_comment_leaves_it_on_its_line() {
    let fx = Fixture::new();
    let mut review = commented_review(&fx, BEFORE, AFTER, "    three();");
    let edited = AFTER.replace("fn b()", "pub fn b()");
    assert_eq!(after_edit(&fx, &mut review, &edited), (Some(6), false));
}

#[test]
fn a_moved_comment_keeps_its_new_place_after_a_reopen() {
    let fx = Fixture::new();
    let mut review = commented_review(&fx, BEFORE, AFTER, "    three();");
    let edited = format!("use std::fmt;\n\n{AFTER}");
    after_edit(&fx, &mut review, &edited);
    let reopened = Review::open(fx.root()).expect("reopen");
    assert_eq!(reopened.session.comments[0].anchor.line, Some(8));
}
