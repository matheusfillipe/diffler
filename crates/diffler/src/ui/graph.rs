//! Graph screen chrome: a hint line, the embedded `diffler_graph::GraphView`,
//! and a status bar. The component draws the graph body; the host draws the
//! chrome and supplies the palette.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, body, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            " hjkl move · n/N edge · ⏎ open/fold · +/- zoom · g/G ends · q back",
            app.theme.dim_style(),
        )),
        hint,
    );
    let gtheme = crate::graph::graph_theme(&app.theme);
    let status = if let Some(graph) = app.graph.as_mut() {
        graph.render(body, frame.buffer_mut(), &gtheme);
        format!(" GRAPH  zoom: {}", graph.zoom().label())
    } else {
        " GRAPH".to_owned()
    };
    let bar_style = Style::new().fg(app.theme.fg).bg(app.theme.panel);
    frame.render_widget(
        Paragraph::new(Line::styled(status, bar_style)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}
