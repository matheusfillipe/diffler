//! Code-review suggestions: the ```` ```suggestion ```` fence GitHub and
//! GitLab offer to apply in place of the lines a comment is anchored to.

use diffler_core::model::DiffModel;
use diffler_core::session::Anchor;

use super::{byte_index, text_edit};

const FENCE: &str = "```suggestion";
const CLOSE: &str = "```";

/// The text of the lines `anchor` covers, on the side of the diff it sits on.
pub(super) fn anchored_lines(model: &DiffModel, anchor: &Anchor) -> Vec<String> {
    let (Some((first, last)), Some(file)) = (
        anchor.span(),
        model.files.iter().find(|file| file.path == anchor.file),
    ) else {
        return Vec::new();
    };
    let mut lines: Vec<(u32, &str)> = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(|line| {
            let number = line.number_on(anchor.on_old_side)?;
            (first..=last)
                .contains(&number)
                .then_some((number, line.text.as_str()))
        })
        .collect();
    lines.sort_by_key(|&(number, _)| number);
    lines.dedup_by_key(|&mut (number, _)| number);
    lines.into_iter().map(|(_, text)| text.to_owned()).collect()
}

/// Insert a suggestion block holding `lines` on lines of its own at `cursor`,
/// leaving the cursor at the start of the first suggested line.
pub(super) fn insert(buffer: &mut String, cursor: &mut usize, lines: &[String]) {
    let mut block = String::new();
    if *cursor > text_edit::line_start(buffer, *cursor) {
        block.push('\n');
    }
    block.push_str(FENCE);
    block.push('\n');
    let first_line = block.chars().count();
    for line in lines {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str(CLOSE);
    if *cursor < text_edit::line_end(buffer, *cursor) {
        block.push('\n');
    }
    buffer.insert_str(byte_index(buffer, *cursor), &block);
    *cursor += first_line;
}

pub(super) fn has_suggestion(body: &str) -> bool {
    body.lines().any(|line| line.trim() == FENCE)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Part<'a> {
    Prose(String),
    Suggestion(Vec<&'a str>),
}

/// `body` cut around its suggestion blocks. An unclosed block runs to the end.
pub(super) fn split(body: &str) -> Vec<Part<'_>> {
    let mut parts = Vec::new();
    let mut prose = String::new();
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        if line.trim() != FENCE {
            prose.push_str(line);
            prose.push('\n');
            continue;
        }
        if !prose.trim().is_empty() {
            parts.push(Part::Prose(std::mem::take(&mut prose)));
        }
        prose.clear();
        let suggested = lines
            .by_ref()
            .take_while(|line| line.trim() != CLOSE)
            .collect();
        parts.push(Part::Suggestion(suggested));
    }
    if !prose.trim().is_empty() {
        parts.push(Part::Prose(prose));
    }
    parts
}

#[cfg(test)]
mod tests {
    use diffler_core::model::{
        BlobIds, DiffLine, FileDiff, FileStatus, HashCache, Hunk, HunkId, LineKind,
    };

    use super::*;

    fn lines(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|&text| text.to_owned()).collect()
    }

    fn inserted(buffer: &str, cursor: usize, block: &[&str]) -> (String, usize) {
        let (mut buffer, mut cursor) = (buffer.to_owned(), cursor);
        insert(&mut buffer, &mut cursor, &lines(block));
        (buffer, cursor)
    }

    #[test]
    fn an_empty_buffer_takes_the_block_with_the_cursor_on_its_first_line() {
        let (buffer, cursor) = inserted("", 0, &["\tlet x = 1;"]);
        assert_eq!(buffer, "```suggestion\n\tlet x = 1;\n```");
        assert_eq!(cursor, "```suggestion\n".len());
    }

    #[test]
    fn a_block_typed_mid_line_breaks_the_line_around_itself() {
        let (buffer, cursor) = inserted("see here", 3, &["a", "b"]);
        assert_eq!(buffer, "see\n```suggestion\na\nb\n```\n here");
        assert_eq!(cursor, "see\n```suggestion\n".len());
    }

    #[test]
    fn a_block_at_a_line_end_starts_on_the_next_line() {
        let (buffer, _) = inserted("why not this?", 13, &["a"]);
        assert_eq!(buffer, "why not this?\n```suggestion\na\n```");
        let (twice, _) = inserted(&buffer, buffer.len(), &["b"]);
        assert!(twice.ends_with("```\n```suggestion\nb\n```"), "{twice}");
    }

    fn line(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: &str) -> DiffLine {
        DiffLine::new(kind, old_no, new_no, text.to_owned())
    }

    fn model() -> DiffModel {
        DiffModel {
            files: vec![FileDiff {
                path: "src/lib.rs".to_owned(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                old_text: None,
                new_text: None,
                hunks: vec![Hunk {
                    id: HunkId("h".to_owned()),
                    old_start: 1,
                    old_lines: 3,
                    new_start: 1,
                    new_lines: 4,
                    context: String::new(),
                    lines: vec![
                        line(LineKind::Context, Some(1), Some(1), "fn answer() -> u32 {"),
                        line(LineKind::Deleted, Some(2), None, "    41"),
                        line(LineKind::Added, None, Some(2), "    42"),
                        line(LineKind::Added, None, Some(3), "    // the answer"),
                        line(LineKind::Context, Some(3), Some(4), "}"),
                    ],
                }],
                hashes: HashCache::default(),
                blobs: BlobIds::default(),
            }],
        }
    }

    fn anchor(line: u32, line_end: Option<u32>, on_old_side: bool) -> Anchor {
        Anchor {
            file: "src/lib.rs".to_owned(),
            line: Some(line),
            line_end,
            on_old_side,
            line_text: None,
            on_file: false,
        }
    }

    #[test]
    fn a_range_takes_every_new_side_line_it_covers() {
        assert_eq!(
            anchored_lines(&model(), &anchor(1, Some(3), false)),
            lines(&["fn answer() -> u32 {", "    42", "    // the answer"])
        );
        assert_eq!(
            anchored_lines(&model(), &anchor(2, None, false)),
            lines(&["    42"])
        );
    }

    #[test]
    fn a_deleted_line_suggests_from_the_old_text() {
        assert_eq!(
            anchored_lines(&model(), &anchor(2, None, true)),
            lines(&["    41"])
        );
    }

    #[test]
    fn a_body_splits_around_its_suggestion() {
        let body = "use this\n```suggestion\n    43\n```\nthanks";
        assert!(has_suggestion(body));
        assert_eq!(
            split(body),
            vec![
                Part::Prose("use this\n".to_owned()),
                Part::Suggestion(vec!["    43"]),
                Part::Prose("thanks\n".to_owned()),
            ]
        );
        assert!(!has_suggestion("```rust\nfn x() {}\n```"));
    }
}
