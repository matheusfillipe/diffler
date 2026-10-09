//! Every kind of figure a card can draw. A ` ```mermaid ` fence is a sequence
//! diagram when its header says so and a flowchart otherwise; a
//! ` ```callstack ` fence is a callstack tree.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::graph::callstack::{self, CallstackError, CallstackFigure};
use crate::graph::mermaid::{self, MermaidError};
use crate::graph::model::{Model, NodeId};
use crate::graph::sequence::{self, SequenceError, SequenceFigure};
use crate::graph::text_figure::TextFigure;
use crate::graph::theme::GraphTheme;
use crate::graph::view::{Fit, GraphView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceKind {
    Mermaid,
    Callstack,
}

impl FenceKind {
    pub fn of(line: &str) -> Option<Self> {
        let rest = line.trim_start().strip_prefix("```")?.trim();
        if rest.eq_ignore_ascii_case("mermaid") {
            Some(Self::Mermaid)
        } else if rest.eq_ignore_ascii_case("callstack") {
            Some(Self::Callstack)
        } else {
            None
        }
    }

    pub fn lang(self) -> &'static str {
        match self {
            Self::Mermaid => "mermaid",
            Self::Callstack => "callstack",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum FigureError {
    #[error(transparent)]
    Mermaid(#[from] MermaidError),
    #[error(transparent)]
    Sequence(#[from] SequenceError),
    #[error(transparent)]
    Callstack(#[from] CallstackError),
}

enum ParsedFigure {
    Flowchart(mermaid::Figure),
    Sequence(SequenceFigure),
    Callstack(CallstackFigure),
}

impl ParsedFigure {
    fn anchors(&self) -> &[(NodeId, String)] {
        match self {
            Self::Flowchart(f) => &f.anchors,
            Self::Sequence(f) => &f.anchors,
            Self::Callstack(f) => &f.anchors,
        }
    }

    fn notes(&self) -> &[String] {
        match self {
            Self::Flowchart(f) => &f.notes,
            Self::Sequence(f) => &f.notes,
            Self::Callstack(f) => &f.notes,
        }
    }
}

fn is_sequence_diagram(src: &str) -> bool {
    mermaid::statements(src)
        .next()
        .is_some_and(mermaid::is_sequence_header)
}

fn parse_fence(kind: FenceKind, src: &str, width: usize) -> Result<ParsedFigure, FigureError> {
    match kind {
        FenceKind::Mermaid if is_sequence_diagram(src) => {
            Ok(ParsedFigure::Sequence(sequence::parse(src, width)?))
        }
        FenceKind::Mermaid => Ok(ParsedFigure::Flowchart(mermaid::parse(src)?)),
        FenceKind::Callstack => Ok(ParsedFigure::Callstack(callstack::parse(src, width)?)),
    }
}

#[derive(Debug)]
pub enum Drawing {
    Graph(Box<GraphView>),
    Text(TextFigure),
}

impl Drawing {
    pub fn height(&self) -> u16 {
        match self {
            Self::Graph(view) => view.height(),
            Self::Text(text) => text.height,
        }
    }

    pub fn width(&self) -> u16 {
        match self {
            Self::Graph(view) => view.width(),
            Self::Text(text) => text.width,
        }
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &GraphTheme) {
        match self {
            Self::Graph(view) => view.render(area, buf, theme),
            Self::Text(text) => text.render(area, buf, theme),
        }
    }

    /// The node a `<cr>` on this row jumps to. A graph has none, since we
    /// open it full screen with `o` to reach its nodes.
    pub fn node_at_row(&self, row: u16) -> Option<&NodeId> {
        match self {
            Self::Graph(_) => None,
            Self::Text(text) => text.node_at_row(row),
        }
    }

    pub fn model(&self) -> Option<&Model> {
        match self {
            Self::Graph(view) => Some(view.model()),
            Self::Text(_) => None,
        }
    }
}

pub struct FigureResult {
    pub drawing: Drawing,
    pub anchors: Vec<(NodeId, String)>,
    pub fit: Fit,
}

pub fn figure(kind: FenceKind, src: &str, width: u16) -> Option<FigureResult> {
    let parsed = parse_fence(kind, src, usize::from(width)).ok()?;
    let anchors = parsed.anchors().to_vec();
    let drawing = match parsed {
        ParsedFigure::Flowchart(figure) => {
            let mut view = GraphView::new();
            let fit = view.set_model_fit(figure.model, width);
            // a card figure is static, so we drop the selection `set_model` gave it
            view.clear_selection();
            return Some(FigureResult {
                drawing: Drawing::Graph(Box::new(view)),
                anchors,
                fit,
            });
        }
        ParsedFigure::Sequence(figure) => figure.text,
        ParsedFigure::Callstack(figure) => figure.text,
    };
    let fit = if drawing.width <= width {
        Fit::AsDrawn
    } else {
        Fit::Cropped
    };
    Some(FigureResult {
        drawing: Drawing::Text(drawing),
        anchors,
        fit,
    })
}

/// Whether `src` draws, and what drawing it simplified, so the MCP reply
/// teaches the agent the supported subset.
pub fn validate_fence(kind: FenceKind, at: usize, src: &str) -> (bool, Vec<String>) {
    // nobody sees this drawing, so we lay it out at zero width
    match parse_fence(kind, src, 0) {
        Ok(figure) => (
            true,
            figure
                .notes()
                .iter()
                .map(|note| format!("diagram {at}: {note}"))
                .collect(),
        ),
        Err(err) => (false, vec![format!("diagram {at}: {err}; left as source")]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_width_card_does_not_panic_for_any_kind() {
        let flowchart =
            figure(FenceKind::Mermaid, "flowchart LR\n  a --> b\n", 0).expect("a flowchart figure");
        assert_eq!(flowchart.fit, Fit::Cropped);

        let sequence = figure(FenceKind::Mermaid, "sequenceDiagram\n  a->>b: hi\n", 0)
            .expect("a sequence figure");
        assert_eq!(sequence.fit, Fit::Cropped);

        let callstack =
            figure(FenceKind::Callstack, "main\n  child\n", 0).expect("a callstack figure");
        assert_eq!(callstack.fit, Fit::Cropped);
    }
}
