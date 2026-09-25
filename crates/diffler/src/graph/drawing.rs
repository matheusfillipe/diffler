//! Every kind of figure a walkthrough card can draw, behind one shape: a
//! navigable flowchart, or a static laid-out-once sequence diagram or
//! callstack tree. A ` ```mermaid ` fence picks between the first two by its
//! own header line; a ` ```callstack ` fence is always the third.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::graph::callstack::{self, CallstackError, CallstackFigure};
use crate::graph::mermaid::{self, MermaidError};
use crate::graph::model::NodeId;
use crate::graph::sequence::{self, SequenceError, SequenceFigure};
use crate::graph::theme::GraphTheme;
use crate::graph::view::{Fit, GraphView};

/// The fence language a stop body names, read straight off its opening
/// ` ``` ` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceKind {
    Mermaid,
    Callstack,
}

impl FenceKind {
    /// The fence language on a ` ``` ` line, or `None` for prose (or a
    /// language this figure system does not draw at all).
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

/// Whether a ` ```mermaid ` fence's first line names a `sequenceDiagram`
/// rather than a flowchart: what routes it to [`sequence::parse`] instead of
/// [`mermaid::parse`], since the two share one fence language.
fn is_sequence_diagram(src: &str) -> bool {
    src.lines()
        .map(|line| line.split_once("%%").map_or(line, |(head, _)| head))
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|first| {
            first
                .split_whitespace()
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("sequenceDiagram"))
        })
}

fn parse_fence(kind: FenceKind, src: &str) -> Result<ParsedFigure, FigureError> {
    match kind {
        FenceKind::Mermaid if is_sequence_diagram(src) => {
            Ok(ParsedFigure::Sequence(sequence::parse(src)?))
        }
        FenceKind::Mermaid => Ok(ParsedFigure::Flowchart(mermaid::parse(src)?)),
        FenceKind::Callstack => Ok(ParsedFigure::Callstack(callstack::parse(src)?)),
    }
}

/// A figure ready to draw in a card: a navigable graph, or a plain laid-out
/// text figure (a sequence diagram or a callstack tree).
#[derive(Debug)]
pub enum Drawing {
    Graph(Box<GraphView>),
    Sequence(crate::graph::text_figure::TextFigure),
    Callstack(crate::graph::text_figure::TextFigure),
}

impl Drawing {
    pub fn height(&self) -> u16 {
        match self {
            Self::Graph(view) => view.height(),
            Self::Sequence(text) | Self::Callstack(text) => text.height,
        }
    }

    pub fn width(&self) -> u16 {
        match self {
            Self::Graph(view) => view.width(),
            Self::Sequence(text) | Self::Callstack(text) => text.width,
        }
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &GraphTheme) {
        match self {
            Self::Graph(view) => view.render(area, buf, theme),
            Self::Sequence(text) | Self::Callstack(text) => text.render(area, buf, theme),
        }
    }

    /// The node a `<cr>` on this row of the drawing jumps to. A graph never
    /// answers here: it is opened full-screen with `o` instead, where every
    /// node is reachable directly.
    pub fn node_at_row(&self, row: u16) -> Option<&NodeId> {
        match self {
            Self::Graph(_) => None,
            Self::Sequence(text) | Self::Callstack(text) => text.node_at_row(row),
        }
    }

    pub fn is_graph(&self) -> bool {
        matches!(self, Self::Graph(_))
    }

    /// The underlying graph model, for a host that wants to hand it to a
    /// fresh full-screen [`GraphView`]. `None` for a figure that is not a
    /// navigable graph at all.
    pub fn model(&self) -> Option<&crate::graph::model::Model> {
        match self {
            Self::Graph(view) => Some(view.model()),
            Self::Sequence(_) | Self::Callstack(_) => None,
        }
    }
}

/// A parsed [`Drawing`], its resolvable anchor targets, and how it fit its
/// card.
pub struct FigureResult {
    pub drawing: Drawing,
    pub anchors: Vec<(NodeId, String)>,
    pub fit: Fit,
}

/// Parse a fence's source into a card-ready [`FigureResult`] fit to `width`
/// columns. `None` for a source this figure system cannot draw at all.
pub fn figure(kind: FenceKind, src: &str, width: u16) -> Option<FigureResult> {
    let parsed = parse_fence(kind, src).ok()?;
    let anchors = parsed.anchors().to_vec();
    let (drawing, fit) = match parsed {
        ParsedFigure::Flowchart(figure) => {
            let mut view = GraphView::new();
            let fit = view.set_model_fit(figure.model, width);
            // a card figure is a static picture, not something being
            // navigated, so it never asked for the default selection
            // `set_model` just gave it
            view.clear_selection();
            (Drawing::Graph(Box::new(view)), fit)
        }
        ParsedFigure::Sequence(figure) => {
            let fit = fit_of(figure.text.width, width);
            (Drawing::Sequence(figure.text), fit)
        }
        ParsedFigure::Callstack(figure) => {
            let fit = fit_of(figure.text.width, width);
            (Drawing::Callstack(figure.text), fit)
        }
    };
    Some(FigureResult {
        drawing,
        anchors,
        fit,
    })
}

fn fit_of(drawn: u16, max_width: u16) -> Fit {
    if drawn <= max_width {
        Fit::AsDrawn
    } else {
        Fit::Cropped
    }
}

/// Figures `src` would draw, and what drawing them simplified: what the MCP
/// write path answers with, so an agent learns the subset without the
/// reader ever seeing a broken figure.
pub fn validate_fence(kind: FenceKind, at: usize, src: &str) -> (bool, Vec<String>) {
    match parse_fence(kind, src) {
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

    /// A card with no width at all still lays every kind out without
    /// panicking: it just crops to nothing.
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
