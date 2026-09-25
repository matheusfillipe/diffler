//! A card figure that draws as plain rows of styled text: a sequence
//! diagram's lanes, a callstack's tree. Both lay themselves out once, at
//! parse time, to the card's width, into this shared shape, so the
//! card renderer and the `<cr>` jump the diff pane gives a figure row treat
//! every kind alike.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::graph::model::NodeId;
use crate::graph::theme::GraphTheme;

/// Which of the theme's colors a span paints over the base (dim) text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// The reader's eye should land here: a participant name, a message
    /// label, an unchanged callstack frame.
    Fg,
    Ok,
    Failed,
}

/// A colored run of `len` cells starting at `(x, y)`, painted over the base
/// text after it renders. Mirrors how [`crate::graph::view::GraphView`]
/// recolors only a node's own cells over its otherwise-dim grid.
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
    /// One entry per row: the node a `<cr>` on that row jumps to, when it
    /// resolved an anchor. `None` for a row with nothing to jump to.
    pub row_nodes: Vec<Option<NodeId>>,
}

/// `text` cut to `width` columns, its last kept cell an ellipsis when it had
/// to be cut at all.
pub(crate) fn elide(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    text.chars().take(width - 1).collect::<String>() + "…"
}

impl TextFigure {
    pub fn node_at_row(&self, row: u16) -> Option<&NodeId> {
        self.row_nodes.get(usize::from(row))?.as_ref()
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &GraphTheme) {
        for (y, line) in self.lines.iter().enumerate() {
            let Ok(y) = u16::try_from(y) else { break };
            if y >= area.height {
                break;
            }
            for (x, ch) in line.chars().enumerate() {
                let Ok(x) = u16::try_from(x) else { break };
                if x >= area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                    cell.set_char(ch);
                    cell.set_fg(theme.dim);
                    cell.set_bg(theme.bg);
                }
            }
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
