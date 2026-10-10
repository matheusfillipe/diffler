//! The agent's reading order for a review. A stop is an agent comment with a
//! title and an anchor. A walkthrough is its own review source, so every
//! comment in that source's session belongs to it.

use serde::{Deserialize, Serialize};

use crate::highlight::Highlighter;
use crate::session::Comment;
use crate::source::ReviewSource;

/// A cap against dumping the whole diff; real walkthroughs stay well under.
pub const MAX_STOPS: usize = 20;
pub const BODY_MAX_BYTES: usize = 8 * 1024;
pub const TOTAL_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Walkthrough {
    pub id: String,
    pub title: String,
    pub author: String,
    pub at: u64,
    /// The comment ids of the stops, in reading order.
    pub stops: Vec<String>,
    /// One line on what the agent left out and why.
    #[serde(default)]
    pub skipped: Option<String>,
    /// Markdown overview, shown as the sidebar's leading slide.
    #[serde(default)]
    pub summary: Option<String>,
    /// Full oid of `HEAD` at publish time, so anchors resolve against the code
    /// they describe after the checkout moves. `None` resolves against the
    /// worktree.
    #[serde(default)]
    pub rev: Option<String>,
    /// The review this walkthrough describes; its diff is what stops resolve
    /// against.
    #[serde(default)]
    pub about: ReviewSource,
}

impl Walkthrough {
    /// The note ids of each stop, in stop order. A note is a titleless comment
    /// with an `anchor_ref` inside a stop's region; it goes to the first stop
    /// that holds it.
    pub fn notes_by_stop(&self, comments: &[Comment]) -> Vec<Vec<String>> {
        let mut groups: Vec<Vec<String>> = self.stops.iter().map(|_| Vec::new()).collect();
        for comment in comments {
            if comment.title.is_some()
                || comment.anchor_ref.is_none()
                || self.stops.contains(&comment.id)
            {
                continue;
            }
            let region = self.stops.iter().position(|stop_id| {
                comments
                    .iter()
                    .find(|c| c.id == *stop_id)
                    .is_some_and(|stop| region_contains(&stop.anchor, &comment.anchor))
            });
            if let Some(group) = region.and_then(|index| groups.get_mut(index)) {
                group.push(comment.id.clone());
            }
        }
        groups
    }
}

/// Whether `other`'s line (or line end) falls inside `region`'s span, on the
/// same file and side. Two file-level anchors match.
pub(crate) fn region_contains(
    region: &crate::session::Anchor,
    other: &crate::session::Anchor,
) -> bool {
    if region.file != other.file || region.on_old_side != other.on_old_side {
        return false;
    }
    match (region.span(), other.line_end.or(other.line)) {
        (Some((start, end)), Some(at)) => start <= at && at <= end,
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptCode {
    TooManyStops,
    EmptyStops,
    BodyTooLong,
    TotalTooLong,
    AnchorUnparsed,
    /// A stop's anchor names a file neither in the diff nor on disk.
    AnchorFileMissing,
    /// No stop names a file and the diff is empty.
    NothingToAnchor,
    NoteOutsideStop,
    DuplicateId,
}

impl ReceiptCode {
    /// The serde wire name.
    pub fn name(self) -> &'static str {
        match self {
            Self::TooManyStops => "too_many_stops",
            Self::EmptyStops => "empty_stops",
            Self::BodyTooLong => "body_too_long",
            Self::TotalTooLong => "total_too_long",
            Self::AnchorUnparsed => "anchor_unparsed",
            Self::AnchorFileMissing => "anchor_file_missing",
            Self::NothingToAnchor => "nothing_to_anchor",
            Self::NoteOutsideStop => "note_outside_stop",
            Self::DuplicateId => "duplicate_id",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub stop: Option<usize>,
    pub code: ReceiptCode,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Located {
    /// 1-based inclusive rows; a single line is a span of one.
    Found {
        line: u32,
        end: u32,
    },
    Whole,
    /// The file was read, but the symbol or line is gone from it.
    Lost,
    /// The file could not be read at all.
    FileMissing,
}

/// Where a figure's node or a stop's anchor points. We resolve it at draw
/// time so it follows the code after an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `path#symbol`, resolved through the file's definition spans.
    Symbol {
        path: String,
        symbol: String,
    },
    /// `path:line` or `path:start-end`, 1-based and inclusive.
    Line {
        path: String,
        line: u32,
        end: u32,
    },
    File {
        path: String,
    },
}

impl Target {
    /// `#` wins over `:`, and both win over a bare path, so a file whose name
    /// contains either is unaddressable.
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        if let Some((path, symbol)) = raw.split_once('#')
            && !symbol.is_empty()
        {
            return Self::Symbol {
                path: path.to_owned(),
                symbol: symbol.to_owned(),
            };
        }
        if let Some((path, rows)) = raw.rsplit_once(':')
            && let Some((line, end)) = parse_rows(rows)
        {
            return Self::Line {
                path: path.to_owned(),
                line,
                end,
            };
        }
        Self::File {
            path: raw.to_owned(),
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::Symbol { path, .. } | Self::Line { path, .. } | Self::File { path } => path,
        }
    }

    pub fn locate(&self, content: &str, highlighter: &Highlighter) -> Located {
        let rows = content.lines().count();
        match self {
            Self::File { .. } => Located::Whole,
            Self::Line { line, end, .. } => {
                if (*line as usize) <= rows {
                    Located::Found {
                        line: *line,
                        end: (*end).min(u32::try_from(rows).unwrap_or(*end)).max(*line),
                    }
                } else {
                    Located::Lost
                }
            }
            Self::Symbol { path, symbol } => highlighter
                .scope_index(path, content)
                .def_span(symbol)
                .and_then(|(start, end)| {
                    Some(Located::Found {
                        line: u32::try_from(start + 1).ok()?,
                        end: u32::try_from(end + 1).ok()?,
                    })
                })
                .unwrap_or(Located::Lost),
        }
    }
}

/// `12` or `12-40`, 1-based and inclusive. A reversed end collapses to the
/// start.
fn parse_rows(raw: &str) -> Option<(u32, u32)> {
    let (start, end) = raw.split_once('-').unwrap_or((raw, raw));
    let start = start.parse::<u32>().ok().filter(|line| *line > 0)?;
    let end = end.parse::<u32>().ok().filter(|line| *line > 0)?;
    Some((start, end.max(start)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_code_name_matches_its_wire_name() {
        for code in [
            ReceiptCode::TooManyStops,
            ReceiptCode::EmptyStops,
            ReceiptCode::BodyTooLong,
            ReceiptCode::TotalTooLong,
            ReceiptCode::AnchorUnparsed,
            ReceiptCode::AnchorFileMissing,
            ReceiptCode::NothingToAnchor,
            ReceiptCode::NoteOutsideStop,
            ReceiptCode::DuplicateId,
        ] {
            let wire = serde_json::to_value(code).expect("a unit enum always serializes");
            assert_eq!(wire, serde_json::json!(code.name()));
        }
    }

    #[test]
    fn walkthrough_serializes_round_trip() {
        let w = Walkthrough {
            id: "w1".to_owned(),
            title: "tour".to_owned(),
            author: "agent".to_owned(),
            at: 1,
            stops: vec!["c1".to_owned(), "c2".to_owned()],
            skipped: Some("the tests".to_owned()),
            summary: Some("what changed".to_owned()),
            rev: Some("deadbeef".to_owned()),
            about: ReviewSource::pr(7),
        };
        let json = serde_json::to_string(&w).expect("serialize");
        let back: Walkthrough = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(w, back);
    }

    #[test]
    fn a_walkthrough_with_no_rev_field_deserializes_to_none() {
        let json = r#"{"id":"w1","title":"tour","author":"agent","at":1,"stops":["c1"]}"#;
        let w: Walkthrough = serde_json::from_str(json).expect("deserialize");
        assert_eq!(w.rev, None);
    }

    #[test]
    fn a_walkthrough_with_no_about_field_deserializes_to_the_working_tree() {
        let json = r#"{"id":"w1","title":"tour","author":"agent","at":1,"stops":["c1"]}"#;
        let w: Walkthrough = serde_json::from_str(json).expect("deserialize");
        assert_eq!(w.about, ReviewSource::WorkingTree);
    }

    fn stop(id: &str, file: &str, line: u32) -> Comment {
        use crate::session::{Anchor, CommentStatus};
        Comment {
            id: id.to_owned(),
            author: "agent".to_owned(),
            remote_id: None,
            thread_id: None,
            anchor: Anchor {
                file: file.to_owned(),
                line: Some(line),
                line_end: None,
                on_old_side: false,
                line_text: None,
            },
            title: Some(id.to_owned()),
            anchor_ref: Some(format!("{file}:{line}")),
            body: "why".to_owned(),
            status: CommentStatus::Open,
            replies: Vec::new(),
            at: 1,
        }
    }

    fn note(id: &str, file: &str, line: u32) -> Comment {
        Comment {
            title: None,
            ..stop(id, file, line)
        }
    }

    #[test]
    fn notes_group_under_the_stop_whose_region_holds_them() {
        let w = Walkthrough {
            id: "w1".to_owned(),
            title: "tour".to_owned(),
            author: "agent".to_owned(),
            at: 1,
            stops: vec!["c1".to_owned(), "c2".to_owned()],
            skipped: None,
            summary: None,
            rev: None,
            about: ReviewSource::WorkingTree,
        };
        let comments = vec![
            stop("c1", "a.txt", 1),
            note("n1", "a.txt", 1),
            note("n2", "a.txt", 1),
            stop("c2", "b.txt", 1),
        ];
        assert_eq!(
            w.notes_by_stop(&comments),
            vec![vec!["n1".to_owned(), "n2".to_owned()], Vec::new()]
        );
    }

    #[test]
    fn a_human_comment_in_a_stops_region_is_never_counted_as_a_note() {
        let w = Walkthrough {
            id: "w1".to_owned(),
            title: "tour".to_owned(),
            author: "agent".to_owned(),
            at: 1,
            stops: vec!["c1".to_owned()],
            skipped: None,
            summary: None,
            rev: None,
            about: ReviewSource::WorkingTree,
        };
        let human = Comment {
            anchor_ref: None,
            ..note("human-1", "a.txt", 1)
        };
        let comments = vec![stop("c1", "a.txt", 1), human];
        assert_eq!(w.notes_by_stop(&comments), vec![Vec::<String>::new()]);
    }

    #[test]
    fn every_target_form_parses() {
        assert_eq!(
            Target::parse("src/config.rs#merge"),
            Target::Symbol {
                path: "src/config.rs".to_owned(),
                symbol: "merge".to_owned()
            }
        );
        assert_eq!(
            Target::parse("src/config.rs:88"),
            Target::Line {
                path: "src/config.rs".to_owned(),
                line: 88,
                end: 88
            }
        );
        assert_eq!(
            Target::parse("src/config.rs"),
            Target::File {
                path: "src/config.rs".to_owned()
            }
        );
    }

    #[test]
    fn a_colon_that_is_not_a_line_number_stays_in_the_path() {
        assert_eq!(
            Target::parse("src/config.rs:"),
            Target::File {
                path: "src/config.rs:".to_owned()
            }
        );
        assert_eq!(
            Target::parse("src/config.rs:0"),
            Target::File {
                path: "src/config.rs:0".to_owned()
            }
        );
    }

    #[test]
    fn a_line_range_parses_and_clamps_to_the_file() {
        assert_eq!(
            Target::parse("src/config.rs:10-20"),
            Target::Line {
                path: "src/config.rs".to_owned(),
                line: 10,
                end: 20
            }
        );
        assert_eq!(
            Target::parse("src/config.rs:20-10"),
            Target::Line {
                path: "src/config.rs".to_owned(),
                line: 20,
                end: 20
            }
        );
        let content = "a\nb\nc\n";
        assert_eq!(
            Target::parse("lib.rs:2-99").locate(content, &Highlighter::default()),
            Located::Found { line: 2, end: 3 },
            "a range past the end clamps instead of going Lost"
        );
    }

    #[test]
    fn a_symbol_resolves_to_the_line_that_defines_it() {
        let content = "fn first() {}\n\nfn merge(a: u8) -> u8 {\n    a\n}\n";
        assert_eq!(
            Target::parse("lib.rs#merge").locate(content, &Highlighter::default()),
            Located::Found { line: 3, end: 5 }
        );
    }

    #[test]
    fn a_symbol_resolves_through_the_readers_language_rule() {
        let content = "fn first() {}\n\nfn merge(a: u8) -> u8 {\n    a\n}\n";
        let target = Target::parse("lib.inc#merge");
        assert_eq!(
            target.locate(content, &Highlighter::default()),
            Located::Lost
        );
        let rules = vec![("*.inc".to_owned(), "rust".to_owned())];
        assert_eq!(
            target.locate(content, &Highlighter::default().with_rules(rules)),
            Located::Found { line: 3, end: 5 }
        );
    }

    #[test]
    fn a_symbol_survives_an_edit_above_it() {
        let before = "fn merge() {}\n";
        let after = "use std::fmt;\n\nfn helper() {}\n\nfn merge() {}\n";
        let target = Target::parse("lib.rs#merge");
        assert_eq!(
            target.locate(before, &Highlighter::default()),
            Located::Found { line: 1, end: 1 }
        );
        assert_eq!(
            target.locate(after, &Highlighter::default()),
            Located::Found { line: 5, end: 5 }
        );
    }

    #[test]
    fn a_symbol_the_file_lost_stops_resolving() {
        assert_eq!(
            Target::parse("lib.rs#gone").locate("fn merge() {}\n", &Highlighter::default()),
            Located::Lost
        );
    }

    #[test]
    fn a_line_past_the_end_stops_resolving() {
        assert_eq!(
            Target::parse("lib.rs:400").locate("fn merge() {}\n", &Highlighter::default()),
            Located::Lost
        );
        assert_eq!(
            Target::parse("lib.rs:1").locate("fn merge() {}\n", &Highlighter::default()),
            Located::Found { line: 1, end: 1 }
        );
    }

    #[test]
    fn a_whole_file_target_locates_the_file_and_no_line() {
        assert_eq!(
            Target::parse("lib.rs").locate("fn merge() {}\n", &Highlighter::default()),
            Located::Whole
        );
    }
}
