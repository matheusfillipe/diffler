//! The walkthrough: the agent's own reading order for a review, one stop per
//! real decision, each a span of code and a short reason. A stop is an agent
//! comment carrying a title and an anchor, so the reader replies to it, the
//! comments pane lists it and the card renderer draws it with no second
//! system beside the first. A walkthrough is a review source of its own
//! (`ReviewSource::Walkthrough`): every comment in that source's session is
//! this walkthrough's, so nothing tracks ownership beyond `stops` itself.

use serde::{Deserialize, Serialize};

use crate::session::Comment;
use crate::syntax::registry::REGISTRY;

/// A rail against dumping the diff, not a target: the skill asks for one stop
/// per real decision, which lands well under this.
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
    /// One line on what the agent left out and why. Surfaced, never hidden.
    #[serde(default)]
    pub skipped: Option<String>,
    /// The walkthrough's own overview: a markdown body exactly like a stop's,
    /// shown as the sidebar's leading slide. `None` for a walkthrough with no
    /// summary, which leaves the sidebar starting at the first stop.
    #[serde(default)]
    pub summary: Option<String>,
    /// The full oid of `HEAD` when this walkthrough was published, so its
    /// anchors can be resolved against the code they actually describe once
    /// the checkout moves on. `None` for a walkthrough saved before this
    /// existed; its anchors resolve against the live worktree.
    #[serde(default)]
    pub rev: Option<String>,
}

impl Walkthrough {
    /// The notes of each stop, in stop order: every comment of the walkthrough's
    /// own session that names no title (so it is a note, not a stop), carries
    /// an anchor of its own (a human comment never does), and whose own anchor
    /// falls inside that stop's region. A note whose region matches more than
    /// one stop goes to the first, the way the agent wrote it; one matching
    /// none (a stop and its own anchor both absent) is not grouped, though it
    /// still exists as a comment.
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

/// Whether `other`'s own anchored line (or line end) falls inside `region`'s
/// span, on the same file and side. Two file-level anchors (no line at all)
/// count as matching, the way a stop with no line holds every other file-level
/// note of the same file. Shared with `store`'s legacy-walkthrough split,
/// which uses the same containment to decide what moves with a stop.
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
    /// A stop's anchor names a file this review has no honest way to reach:
    /// not in the diff, and not readable on disk either.
    AnchorFileMissing,
    /// No stop names a file and the diff itself is empty, so an anchorless
    /// stop has nothing real to fall back on.
    NothingToAnchor,
    NoteOutsideStop,
    DuplicateId,
}

impl ReceiptCode {
    /// The name serde gives this code on the wire, so a diagnostic printed
    /// for a human matches what an agent reads back from the tool call.
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

/// The outcome of pointing a target at a file's current contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Located {
    /// The rows the reference covers, 1-based and inclusive. A reference to a
    /// single line is a span of one, so the reader always gets a segment.
    Found { line: u32, end: u32 },
    /// A whole-file target: the file, at no particular line.
    Whole,
    /// The file was read, but the symbol or line the target names is gone
    /// from it.
    Lost,
    /// The file itself could not be read at all: absent from the revision
    /// the target was resolved against (or, with none pinned, from the
    /// worktree). Distinguished from `Lost` so the reader is told which one
    /// happened.
    FileMissing,
}

/// Where a figure's node, or a stop's anchor, points. Resolution happens
/// when it is drawn, never when it is written, so it keeps pointing at the
/// right code after an edit moves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `path#symbol`, resolved through the file's definition spans. The form
    /// to prefer: it survives the symbol moving.
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
    /// contains either is unaddressable. Naming a symbol is worth more than
    /// serving a path POSIX allows and no repo uses.
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

    /// Where in `content` the target lands, in one pass: `Found(line)` for a
    /// target that names a line, `Whole` for a file, `Lost` for a symbol the
    /// file no longer defines. One call, since resolving a symbol parses the
    /// file and asking twice parses it twice.
    pub fn locate(&self, content: &str) -> Located {
        let rows = content.lines().count();
        match self {
            Self::File { .. } => Located::Whole,
            Self::Line { line, end, .. } => {
                if (*line as usize) <= rows {
                    // a range running off the end still opens, clamped: the
                    // file shrank under the reference, it did not move
                    Located::Found {
                        line: *line,
                        end: (*end).min(u32::try_from(rows).unwrap_or(*end)).max(*line),
                    }
                } else {
                    Located::Lost
                }
            }
            Self::Symbol { path, symbol } => REGISTRY
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

/// `12` or `12-40`, both 1-based and inclusive. A reversed or absent end
/// collapses to the start, so every accepted form yields a usable span.
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
        };
        let json = serde_json::to_string(&w).expect("serialize");
        let back: Walkthrough = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(w, back);
    }

    /// A walkthrough saved before `rev` existed has no such key at all; it
    /// still loads, with `rev` defaulting to `None`.
    #[test]
    fn a_walkthrough_with_no_rev_field_deserializes_to_none() {
        let json = r#"{"id":"w1","title":"tour","author":"agent","at":1,"stops":["c1"]}"#;
        let w: Walkthrough = serde_json::from_str(json).expect("deserialize");
        assert_eq!(w.rev, None);
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

    /// A note belongs to the stop whose region its own anchor falls inside.
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

    /// A human comment carries no `anchor_ref`, so it is never mistaken for a
    /// note even when it sits inside a stop's own region.
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

    /// A windows path, and a trailing colon with no number, are files: a bad
    /// split would point the node at a path that does not exist.
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

    /// `path:start-end` is how an agent points at a segment it can see but
    /// cannot name: a block inside a function, a stanza of config.
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
        // a reversed range is the reader's typo, not a reason to refuse
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
            Target::parse("lib.rs:2-99").locate(content),
            Located::Found { line: 2, end: 3 },
            "a range past the end clamps instead of going Lost"
        );
    }

    #[test]
    fn a_symbol_resolves_to_the_line_that_defines_it() {
        let content = "fn first() {}\n\nfn merge(a: u8) -> u8 {\n    a\n}\n";
        // the span runs to the end of the definition, so opening it shows the
        // whole function rather than seating a cursor on its signature
        assert_eq!(
            Target::parse("lib.rs#merge").locate(content),
            Located::Found { line: 3, end: 5 }
        );
    }

    /// The point of anchoring to a symbol: an edit above it moves the line and
    /// the target still finds it.
    #[test]
    fn a_symbol_survives_an_edit_above_it() {
        let before = "fn merge() {}\n";
        let after = "use std::fmt;\n\nfn helper() {}\n\nfn merge() {}\n";
        let target = Target::parse("lib.rs#merge");
        assert_eq!(target.locate(before), Located::Found { line: 1, end: 1 });
        assert_eq!(target.locate(after), Located::Found { line: 5, end: 5 });
    }

    #[test]
    fn a_symbol_the_file_lost_stops_resolving() {
        assert_eq!(
            Target::parse("lib.rs#gone").locate("fn merge() {}\n"),
            Located::Lost
        );
    }

    #[test]
    fn a_line_past_the_end_stops_resolving() {
        assert_eq!(
            Target::parse("lib.rs:400").locate("fn merge() {}\n"),
            Located::Lost
        );
        assert_eq!(
            Target::parse("lib.rs:1").locate("fn merge() {}\n"),
            Located::Found { line: 1, end: 1 }
        );
    }

    #[test]
    fn a_whole_file_target_locates_the_file_and_no_line() {
        assert_eq!(
            Target::parse("lib.rs").locate("fn merge() {}\n"),
            Located::Whole
        );
    }
}
