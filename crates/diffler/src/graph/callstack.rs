//! A ` ```callstack ` fence: the old versus new call path as one tree,
//! diff-like so an agent already knows the syntax. One frame per line: an
//! optional `+`/`-` marker, two-space indentation per depth, the frame's
//! label, and an optional ` @ <anchor>` naming the code it calls into.
//!
//! ```text
//! main
//!   handle_request
//!   - legacy_auth @ src/auth.rs#legacy_auth
//!   + new_auth @ src/auth.rs#new_auth
//! ```

use unicode_width::UnicodeWidthStr;

use crate::graph::model::NodeId;
use crate::graph::text_figure::{SpanKind, TextFigure, TextSpan, elide};

/// Frames one tree may hold, mirroring [`crate::graph::mermaid::MAX_NODES`]:
/// past this a terminal card cannot read it anyway, and the text the tree is
/// built from comes from an agent.
pub(crate) const MAX_FRAMES: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Added,
    Removed,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    depth: usize,
    marker: Marker,
    label: String,
    anchor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CallstackError {
    #[error("the callstack names no frames")]
    Empty,
}

#[derive(Debug, Clone)]
pub(crate) struct CallstackFigure {
    pub text: TextFigure,
    pub anchors: Vec<(NodeId, String)>,
    pub notes: Vec<String>,
}

/// Parse and draw a callstack tree, each label elided so its row fits
/// `max_width` columns.
pub(crate) fn parse(src: &str, max_width: usize) -> Result<CallstackFigure, CallstackError> {
    let mut frames = Vec::new();
    let mut notes = Vec::new();
    for raw in src.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        match parse_line(raw) {
            Some(frame) => frames.push(frame),
            None => notes.push(format!("a line named no frame and was skipped: {raw:?}")),
        }
    }
    if frames.is_empty() {
        return Err(CallstackError::Empty);
    }
    if frames.len() > MAX_FRAMES {
        frames.truncate(MAX_FRAMES);
        notes.push(format!("only the first {MAX_FRAMES} frames are drawn"));
    }
    // a fence indented as a whole (inside a list, say) still roots at its
    // least indented frame, and a depth jumping more than one level past its
    // predecessor still attaches somewhere sane
    let base = frames.iter().map(|f| f.depth).min().unwrap_or(0);
    let mut previous_depth: Option<usize> = None;
    for frame in &mut frames {
        let depth = (frame.depth - base) / 2;
        frame.depth = previous_depth.map_or(0, |previous| depth.min(previous + 1));
        previous_depth = Some(frame.depth);
    }

    let mut anchors = Vec::new();
    let mut row_nodes = Vec::with_capacity(frames.len());
    for (index, frame) in frames.iter().enumerate() {
        match &frame.anchor {
            Some(anchor) => {
                let id = NodeId::new(format!("frame{index}"));
                anchors.push((id.clone(), anchor.clone()));
                row_nodes.push(Some(id));
            }
            None => row_nodes.push(None),
        }
    }

    Ok(CallstackFigure {
        text: render(&frames, row_nodes, max_width),
        anchors,
        notes,
    })
}

/// `[<marker> ]<label>[ @ <anchor>]`, indented two spaces per depth. A marker
/// is only recognized as `+ `/`- ` (with the trailing space): a label that
/// merely starts with either character stays a label. The frame's `depth`
/// here is its raw indent in columns, a tab counting as one level.
fn parse_line(raw: &str) -> Option<Frame> {
    let trimmed_start = raw.trim_start_matches([' ', '\t']);
    let indent: usize = raw[..raw.len() - trimmed_start.len()]
        .chars()
        .map(|c| if c == '\t' { 2 } else { 1 })
        .sum();
    let (marker, rest) = if let Some(after) = trimmed_start.strip_prefix("+ ") {
        (Marker::Added, after)
    } else if let Some(after) = trimmed_start.strip_prefix("- ") {
        (Marker::Removed, after)
    } else {
        (Marker::Unchanged, trimmed_start)
    };
    let (label, anchor) = match rest.rsplit_once(" @ ") {
        Some((label, anchor)) => (label.trim(), Some(anchor.trim().to_owned())),
        None => (rest.trim(), None),
    };
    if label.is_empty() {
        return None;
    }
    Some(Frame {
        depth: indent,
        marker,
        label: label.to_owned(),
        anchor,
    })
}

/// Whether each frame is the last child among its siblings: the nearest
/// following frame at the same depth exists before one shallower.
fn compute_last(frames: &[Frame]) -> Vec<bool> {
    (0..frames.len())
        .map(|index| {
            let Some(frame) = frames.get(index) else {
                return true;
            };
            !frames
                .iter()
                .skip(index + 1)
                .take_while(|later| later.depth >= frame.depth)
                .any(|later| later.depth == frame.depth)
        })
        .collect()
}

/// Draw the tree with box-drawing connectors: `├─`/`└─` per frame, `│` for an
/// ancestor level with more siblings still to come, blank where it does not.
fn render(frames: &[Frame], row_nodes: Vec<Option<NodeId>>, max_width: usize) -> TextFigure {
    let is_last = compute_last(frames);
    let mut ancestor_last: Vec<bool> = Vec::new();
    let mut lines = Vec::with_capacity(frames.len());
    let mut spans = Vec::with_capacity(frames.len());

    for (index, (frame, &last_child)) in frames.iter().zip(&is_last).enumerate() {
        let mut prefix = String::new();
        if frame.depth == 0 {
            // a root draws no rail of its own; its children start clean
            ancestor_last.clear();
        } else {
            // rails come from every ancestor strictly between the root and
            // this frame's own parent; the parent's own connector merges
            // into this frame's, not into the rail above it
            ancestor_last.truncate(frame.depth - 1);
            for &last in &ancestor_last {
                prefix.push_str(if last { "   " } else { "│  " });
            }
            prefix.push_str(if last_child { "└─ " } else { "├─ " });
            ancestor_last.push(last_child);
        }
        let marker_glyph = match frame.marker {
            Marker::Added => "+ ",
            Marker::Removed => "- ",
            Marker::Unchanged => "",
        };
        let prefix_len = prefix.width() + marker_glyph.width();
        let label = elide(&frame.label, max_width.saturating_sub(prefix_len).max(1));
        let content_len = u16::try_from(marker_glyph.width() + label.width()).unwrap_or(u16::MAX);
        spans.push(TextSpan {
            x: u16::try_from(prefix.width()).unwrap_or(0),
            y: u16::try_from(index).unwrap_or(u16::MAX),
            len: content_len,
            kind: match frame.marker {
                Marker::Added => SpanKind::Ok,
                Marker::Removed => SpanKind::Failed,
                Marker::Unchanged => SpanKind::Fg,
            },
        });
        lines.push(format!("{prefix}{marker_glyph}{label}"));
    }

    let width = lines.iter().map(|line| line.width()).max().unwrap_or(0);
    TextFigure {
        width: u16::try_from(width).unwrap_or(u16::MAX),
        height: u16::try_from(lines.len()).unwrap_or(u16::MAX),
        lines,
        spans,
        row_nodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn figure(src: &str) -> CallstackFigure {
        parse(src, usize::MAX).expect("parsed")
    }

    /// A fence indented as a whole roots at its least indented frame, and a
    /// tab counts as one level.
    #[test]
    fn an_indented_fence_and_tabs_root_at_the_least_indented_frame() {
        let figure = figure("    main\n      first\n\t\t\t\tsecond");
        assert_eq!(figure.text.lines, ["main", "└─ first", "   └─ second"]);
    }

    #[test]
    fn a_label_holding_an_at_sign_keeps_it_and_the_anchor_is_the_last_one() {
        let figure = figure("main\n  on @ event @ src/ev.rs#on");
        assert_eq!(figure.text.lines[1], "└─ on @ event");
        assert_eq!(figure.anchors[0].1, "src/ev.rs#on");
    }

    #[test]
    fn a_label_too_long_for_the_card_is_elided() {
        let figure = parse(&format!("main\n  + {}", "x".repeat(80)), 20).expect("parsed");
        assert_eq!(figure.text.lines[1].chars().count(), 20);
        assert!(figure.text.lines[1].ends_with('…'));
    }

    /// A CJK label is twice as wide on screen as it is long in characters: the
    /// figure's own `width` and the row's `TextSpan::len` have to reflect
    /// that, not the character count, or the card crops nothing and a
    /// narrower card overflows.
    #[test]
    fn a_cjk_label_is_sized_and_elided_by_display_width() {
        let figure = parse("main\n  部署完成流程说明", 12).expect("parsed");
        let row = &figure.text.lines[1];
        assert!(row.ends_with('…'), "{row}");
        assert_eq!(
            figure.text.width, 12,
            "the figure reports its true cell width"
        );
        let span = figure.text.spans[1];
        assert_eq!(
            usize::from(span.x) + usize::from(span.len),
            row.width(),
            "the span covers exactly the row's own display width"
        );
    }

    #[test]
    fn crlf_and_trailing_spaces_parse_like_plain_lines() {
        let figure = figure("main  \r\n  child @ src/a.rs#child  \r\n");
        assert_eq!(figure.text.lines, ["main", "└─ child"]);
        assert_eq!(figure.anchors[0].1, "src/a.rs#child");
    }

    #[test]
    fn a_flat_callstack_draws_one_row_per_frame() {
        let figure = figure("main\n  handle_request\n  respond");
        assert_eq!(figure.text.lines.len(), 3);
        assert_eq!(figure.text.lines[0], "main");
        assert_eq!(figure.text.lines[1], "├─ handle_request");
        assert_eq!(figure.text.lines[2], "└─ respond");
        assert!(figure.notes.is_empty(), "{:?}", figure.notes);
    }

    #[test]
    fn a_middle_child_gets_a_tee_a_last_child_a_corner() {
        let figure = figure("main\n  first\n  second");
        assert_eq!(figure.text.lines[1], "├─ first");
        assert_eq!(figure.text.lines[2], "└─ second");
    }

    #[test]
    fn markers_color_added_and_removed_frames() {
        let figure = figure("main\n  + new_auth\n  - legacy_auth");
        assert_eq!(figure.text.lines[1], "├─ + new_auth");
        assert_eq!(figure.text.lines[2], "└─ - legacy_auth");
        assert_eq!(figure.text.spans[1].kind, SpanKind::Ok);
        assert_eq!(figure.text.spans[2].kind, SpanKind::Failed);
    }

    #[test]
    fn an_anchor_after_at_becomes_a_jumpable_row() {
        let figure = figure("main\n  handle @ src/http.rs#handle");
        assert_eq!(
            figure.anchors,
            [(NodeId::new("frame1"), "src/http.rs#handle".to_owned())]
        );
        assert_eq!(figure.text.node_at_row(1), Some(&NodeId::new("frame1")));
        assert_eq!(figure.text.node_at_row(0), None);
    }

    #[test]
    fn a_grandchild_keeps_the_rail_under_an_open_sibling() {
        let figure = figure("main\n  first\n    inner\n  second");
        // `first` is not the last child (`second` follows), so its own
        // child's row carries a continuing rail, not a blank gap
        assert_eq!(figure.text.lines[2], "│  └─ inner");
        assert_eq!(figure.text.lines[3], "└─ second");
    }

    #[test]
    fn an_empty_callstack_is_an_error() {
        assert_eq!(parse("", 80).unwrap_err(), CallstackError::Empty);
        assert_eq!(parse("   \n  \n", 80).unwrap_err(), CallstackError::Empty);
    }

    #[test]
    fn a_line_with_only_a_marker_is_skipped_and_noted() {
        let figure = figure("main\n  + \n  child");
        assert_eq!(figure.text.lines.len(), 2);
        assert!(figure.notes.iter().any(|n| n.contains("skipped")));
    }

    #[test]
    fn a_depth_jump_clamps_to_one_level_deeper() {
        let figure = figure("main\n      deeply_indented");
        assert_eq!(figure.text.lines[1], "└─ deeply_indented");
    }

    #[test]
    fn a_callstack_past_the_frame_cap_is_truncated_and_says_so() {
        use std::fmt::Write as _;
        let mut src = String::from("main\n");
        for index in 0..(MAX_FRAMES + 5) {
            let _ = writeln!(src, "  frame{index}");
        }
        let figure = figure(&src);
        assert_eq!(figure.text.lines.len(), MAX_FRAMES);
        assert_eq!(
            figure.notes,
            [format!("only the first {MAX_FRAMES} frames are drawn")]
        );
    }

    #[test]
    fn a_deep_callstack_does_not_panic() {
        use std::fmt::Write as _;
        let mut src = String::new();
        for (indent, index) in (0..40).enumerate() {
            src.push_str(&"  ".repeat(indent));
            let _ = writeln!(src, "frame{index}");
        }
        let figure = figure(&src);
        assert_eq!(figure.text.lines.len(), 40);
    }

    #[test]
    fn a_very_long_label_does_not_panic() {
        let long = "x".repeat(500);
        let figure = figure(&format!("main\n  {long}"));
        assert!(figure.text.lines[1].contains(&long));
    }
}
