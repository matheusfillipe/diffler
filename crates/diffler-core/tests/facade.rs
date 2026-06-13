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
        hunk: None,
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
    assert_eq!(review.model.files.len(), 1);
    assert_eq!(review.model.files[0].path, "a.py");
    assert_eq!(review.status.unstaged.files.len(), 1);

    let hash = review.model.files[0].content_hash();
    review
        .session
        .add_comment("mattf", anchor("a.py", 2), "why 2?");
    review.session.mark_viewed("a.py", &hash);
    review.save().expect("save");

    // reopen: comment persisted, viewed mark still valid for current content
    let mut review = Review::open(fx.root()).expect("reopen");
    assert_eq!(review.session.comments.len(), 1);
    assert_eq!(review.session.comments[0].body, "why 2?");
    let current = review.model.files[0].content_hash();
    assert!(review.session.is_viewed("a.py", &current));

    // the agent rewrites the file: viewed auto-resets, the comment stays
    fx.write("a.py", "def f():\n    return 3\n");
    review.refresh().expect("refresh");
    assert!(review.session.viewed.is_empty());
    let current = review.model.files[0].content_hash();
    assert!(!review.session.is_viewed("a.py", &current));
    assert_eq!(review.session.comments.len(), 1);
}
