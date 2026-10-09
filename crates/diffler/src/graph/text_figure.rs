//! A card figure drawn as rows of styled text, laid out once at parse time to
//! the card's width. Sequence diagrams and callstacks share this shape so the
//! card renderer and the `<cr>` jump treat them alike.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::graph::model::NodeId;
use crate::graph::theme::GraphTheme;

/// Which of the theme's colors a span paints over the base (dim) text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Fg,
    Ok,
    Failed,
}

/// A colored run of `len` cells starting at `(x, y)`, painted over the base
/// text after it renders.
#[derive(Debug, Clone, Copy)]
pub struct TextSpan {
    pub x: u16,
    pub y: u16,
    pub len: u16,
    pub kind: SpanKind,
}

/// A figure laid out as plain text: every character already placed, colored
/// only where a [`TextSpan`] says so.
#[derive(Debug, Clone, Default)]
pub struct TextFigure {
    pub lines: Vec<String>,
    pub width: u16,
    pub height: u16,
    pub spans: Vec<TextSpan>,
    /// One entry per row: the node a `<cr>` on that row jumps to.
    pub row_nodes: Vec<Option<NodeId>>,
}

pub(crate) use crate::text::elide;

impl TextFigure {
    pub fn node_at_row(&self, row: u16) -> Option<&NodeId> {
        self.row_nodes.get(usize::from(row))?.as_ref()
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &GraphTheme) {
        let style = Style::new().fg(theme.dim).bg(theme.bg);
        for (y, line) in self.lines.iter().enumerate() {
            let Ok(y) = u16::try_from(y) else { break };
            if y >= area.height {
                break;
            }
            buf.set_stringn(area.x, area.y + y, line, usize::from(area.width), style);
        }
        for span in &self.spans {
            if span.y >= area.height {
                continue;
            }
            let color = match span.kind {
                SpanKind::Fg => theme.fg,
                SpanKind::Ok => theme.ok,
                SpanKind::Failed => theme.failed,
            };
            for dx in 0..span.len {
                let x = span.x + dx;
                if x >= area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((area.x + x, area.y + span.y)) {
                    cell.set_fg(color);
                }
            }
        }
    }
}
