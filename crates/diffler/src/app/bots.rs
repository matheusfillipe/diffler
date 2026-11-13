//! Hiding bot comments, so a review can focus on what people wrote.

use std::borrow::Cow;

use diffler_core::session::{Comment, Session};

use crate::ci::PrNote;
use crate::config::ReviewConfig;

/// What the review screens hide, one copy per view so a draw needs no `App`.
#[derive(Debug, Clone, Default)]
pub struct BotFilter {
    pub review: ReviewConfig,
}

impl BotFilter {
    fn hiding(&self) -> bool {
        self.review.hide_bots
    }

    pub fn is_bot(&self, author: &str) -> bool {
        self.review.is_bot(author)
    }

    /// A thread a bot started stays while a person answered in it.
    pub fn hides_thread(&self, comment: &Comment) -> bool {
        self.hiding()
            && self.is_bot(&comment.author)
            && comment
                .replies
                .iter()
                .all(|reply| self.is_bot(&reply.author))
    }

    /// The same rule for a PR's conversation, where a thread is a root note
    /// and the notes whose `reply_to` names it.
    pub fn hides_note(&self, note: &PrNote, notes: &[PrNote]) -> bool {
        if !self.hiding() || !self.is_bot(&note.author) {
            return false;
        }
        note.reply_to.is_some()
            || notes
                .iter()
                .filter(|other| other.reply_to.as_ref() == Some(&note.id))
                .all(|reply| self.is_bot(&reply.author))
    }

    /// `comment` with the bot replies it hides left out.
    pub fn shown<'a>(&self, comment: &'a Comment) -> Cow<'a, Comment> {
        if !self.hiding() || !comment.replies.iter().any(|r| self.is_bot(&r.author)) {
            return Cow::Borrowed(comment);
        }
        let mut shown = comment.clone();
        shown.replies.retain(|reply| !self.is_bot(&reply.author));
        Cow::Owned(shown)
    }

    /// Bot comments and replies out of sight: whole bot threads, and bot
    /// replies in the threads that stay.
    pub fn hidden_count(&self, session: &Session) -> usize {
        if !self.hiding() {
            return 0;
        }
        session
            .comments
            .iter()
            .map(|comment| {
                if self.hides_thread(comment) {
                    1 + comment.replies.len()
                } else {
                    comment
                        .replies
                        .iter()
                        .filter(|reply| self.is_bot(&reply.author))
                        .count()
                }
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use diffler_core::session::{Anchor, CommentStatus, Reply};

    use super::*;

    fn comment(author: &str, replies: &[&str]) -> Comment {
        Comment {
            id: format!("c-{author}"),
            author: author.to_owned(),
            remote_id: None,
            thread_id: None,
            anchor: Anchor {
                file: "a.rs".to_owned(),
                line: Some(1),
                line_end: None,
                on_old_side: false,
                line_text: None,
                on_file: false,
            },
            title: None,
            anchor_ref: None,
            body: "body".to_owned(),
            status: CommentStatus::Open,
            replies: replies
                .iter()
                .map(|author| Reply {
                    author: (*author).to_owned(),
                    body: "reply".to_owned(),
                    at: 0,
                    remote_id: None,
                })
                .collect(),
            at: 0,
        }
    }

    fn hiding(extra: &[&str]) -> BotFilter {
        BotFilter {
            review: ReviewConfig {
                hide_bots: true,
                bots: extra.iter().map(|bot| (*bot).to_owned()).collect(),
            },
        }
    }

    #[test]
    fn a_bot_thread_hides_until_a_person_answers_it() {
        let filter = hiding(&[]);
        assert!(filter.hides_thread(&comment("Copilot[bot]", &[])));
        assert!(filter.hides_thread(&comment("Copilot[bot]", &["coderabbitai[bot]"])));
        assert!(!filter.hides_thread(&comment("Copilot[bot]", &["reviewer"])));
        assert!(!filter.hides_thread(&comment("reviewer", &[])));
    }

    #[test]
    fn listed_accounts_count_as_bots_and_nothing_hides_while_off() {
        let filter = hiding(&["release-helper"]);
        assert!(filter.hides_thread(&comment("release-helper", &[])));
        let off = BotFilter {
            review: ReviewConfig {
                hide_bots: false,
                ..filter.review.clone()
            },
        };
        assert!(!off.hides_thread(&comment("release-helper", &[])));
        let session = Session {
            comments: vec![comment("release-helper", &[])],
            ..Session::default()
        };
        assert_eq!(off.hidden_count(&session), 0);
    }

    #[test]
    fn a_person_thread_keeps_its_people_and_counts_the_bot_replies_it_drops() {
        let filter = hiding(&[]);
        let thread = comment(
            "reviewer",
            &["Copilot[bot]", "author", "github-actions[bot]"],
        );
        let shown = filter.shown(&thread);
        let authors: Vec<&str> = shown.replies.iter().map(|r| r.author.as_str()).collect();
        assert_eq!(authors, ["author"]);
        let session = Session {
            comments: vec![thread, comment("Copilot[bot]", &[])],
            ..Session::default()
        };
        assert_eq!(filter.hidden_count(&session), 3);
    }

    fn note(id: &str, author: &str, reply_to: Option<&str>) -> PrNote {
        PrNote {
            id: id.to_owned(),
            author: author.to_owned(),
            body: "note".to_owned(),
            at: 0,
            verdict: None,
            thread_id: None,
            reply_to: reply_to.map(str::to_owned),
        }
    }

    #[test]
    fn a_bot_note_a_person_answered_keeps_its_place_above_the_answer() {
        let filter = hiding(&[]);
        let notes = [
            note("1", "project_4_bot_ab[bot]", None),
            note("2", "reviewer", Some("1")),
            note("3", "project_4_bot_ab[bot]", Some("1")),
            note("4", "project_4_bot_ab[bot]", None),
        ];
        let hidden: Vec<&str> = notes
            .iter()
            .filter(|n| filter.hides_note(n, &notes))
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(hidden, ["3", "4"]);
    }
}
