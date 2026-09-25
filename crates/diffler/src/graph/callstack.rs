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

use crate::graph::model::NodeId;
use crate::graph::text_figure::{SpanKind, TextFigure, TextSpan};

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

pub(crate) fn parse(src: &str) -> Result<CallstackFigure, CallstackError> {
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
    // a depth jumping more than one level past its predecessor (a typo, or a
    // fence missing an ancestor) still attaches somewhere sane rather than
    // stranding the tree walk
    let mut previous_depth = 0usize;
    for frame in &mut frames {
        frame.depth = frame.depth.min(previous_depth + 1);
        previous_depth = frame.depth;
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
        text: render(&frames, row_nodes),
        anchors,
        notes,
    })
}

/// `[<marker> ]<label>[ @ <anchor>]`, indented two spaces per depth. A marker
/// is only recognized as `+ `/`- ` (with the trailing space): a label that
/// merely starts with either character stays a label.
fn parse_line(raw: &str) -> Option<Frame> {
    let trimmed_start = raw.trim_start_matches(' ');
    let depth = (raw.len() - trimmed_start.len()) / 2;
    let (marker, rest) = if let Some(after) = trimmed_start.strip_prefix("+ ") {
        (Marker::Added, after)
    } else if let Some(after) = trimmed_start.strip_prefix("- ") {
        (Marker::Removed, after)
    } else {
        (Marker::Unchanged, trimmed_start)
    };
    let (label, anchor) = match rest.split_once(" @ ") {
        Some((label, anchor)) => (label.trim(), Some(anchor.trim().to_owned())),
        None => (rest.trim(), None),
    };
    if label.is_empty() {
        return None;
    }
    Some(Frame {
        depth,
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
fn render(frames: &[Frame], row_nodes: Vec<Option<NodeId>>) -> TextFigure {
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
        let prefix_len = u16::try_from(prefix.chars().count()).unwrap_or(0);
        let content_len = u16::try_from(marker_glyph.chars().count() + frame.label.chars().count())
            .unwrap_or(u16::MAX);
        spans.push(TextSpan {
            x: prefix_len,
            y: u16::try_from(index).unwrap_or(u16::MAX),
            len: content_len,
            kind: match frame.marker {
                Marker::Added => SpanKind::Ok,
                Marker::Removed => SpanKind::Failed,
                Marker::Unchanged => SpanKind::Fg,
            },
        });
        lines.push(format!("{prefix}{marker_glyph}{}", frame.label));
    }

    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
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
        parse(src).expect("parsed")
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
        assert_eq!(parse("").unwrap_err(), CallstackError::Empty);
        assert_eq!(parse("   \n  \n").unwrap_err(), CallstackError::Empty);
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
