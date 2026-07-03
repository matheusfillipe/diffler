//! Graph screen chrome: a hint line, the embedded `crate::graph::GraphView`,
//! and a status bar. The component draws the graph body; the host draws the
//! chrome and supplies the palette. When the open run has artifacts or
//! annotations, a read-only panel for them is carved off the bottom of the body
//! — the graph keeps the full body otherwise, so it renders exactly as before.

use crate::ci::{AnnotationLevel, RunExtras};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;
use crate::theme::Theme;

/// Body rows below which the extras panel is suppressed, so a short terminal
/// leaves the whole body to the graph.
const MIN_BODY_FOR_PANEL: u16 = 9;

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(app.theme.base()), area);
    let [hint, header, body, bar] = Layout::vertical([
        Constraint::Length(1),
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
    frame.render_widget(Paragraph::new(run_header(app, &app.theme)), header);

    let (graph_area, panel) = carve_panel(app, body);

    let gtheme = crate::graph::graph_theme(&app.theme);
    let status = if let Some(graph) = app.graph.as_mut() {
        graph.render(graph_area, frame.buffer_mut(), &gtheme);
        format!(" GRAPH  zoom: {}", graph.zoom().label())
    } else {
        " GRAPH".to_owned()
    };

    if let Some((rect, lines)) = panel {
        render_panel(frame, rect, lines, &app.theme);
    }

    let bar_style = Style::new().fg(app.theme.fg).bg(app.theme.panel);
    frame.render_widget(
        Paragraph::new(Line::styled(status, bar_style)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

/// One-line provenance for the open run: where it ran, which workflow,
/// what commit — the graph alone doesn't say what you're looking at.
pub(crate) fn run_header(app: &App, theme: &Theme) -> Line<'static> {
    let Some(run) = app.open_run_summary() else {
        return Line::default();
    };
    let source = match &run.remote {
        Some(remote) => format!(" {remote}/{}", run.name),
        None => format!(" {}", run.name),
    };
    let commit: String = run.commit.chars().take(7).collect();
    let mut spans = vec![Span::styled(source, Style::new().fg(theme.accent))];
    spans.push(Span::styled(
        format!("  {}", run.branch),
        Style::new().fg(theme.fg),
    ));
    spans.push(Span::styled(format!(" @ {commit}"), theme.dim_style()));
    if !run.title.is_empty() {
        spans.push(Span::styled(format!("  {}", run.title), theme.dim_style()));
    }
    Line::from(spans)
}

/// Split `body` into the graph area and, when the open run has extras and the
/// body is tall enough, a bottom panel rect with its rendered lines. With no
/// extras the graph keeps the whole body, so the DAG renders exactly as before.
fn carve_panel(app: &App, body: Rect) -> (Rect, Option<(Rect, Vec<Line<'static>>)>) {
    let lines = app
        .extras
        .as_ref()
        .filter(|e| !e.artifacts.is_empty() || !e.annotations.is_empty())
        .filter(|_| body.height >= MIN_BODY_FOR_PANEL)
        .map(|extras| extras_lines(extras, &app.theme));
    match lines {
        Some(lines) => {
            let want = u16::try_from(lines.len())
                .unwrap_or(u16::MAX)
                .saturating_add(2)
                .min(body.height / 2);
            let [graph_area, panel] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(want)]).areas(body);
            (graph_area, Some((panel, lines)))
        }
        None => (body, None),
    }
}

fn render_panel(frame: &mut Frame<'_>, rect: Rect, lines: Vec<Line<'static>>, theme: &Theme) {
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(theme.border))
        .title(Span::styled(" run extras ", theme.dim_style()));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(lines).style(theme.base()), inner);
}

fn extras_lines(extras: &RunExtras, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !extras.artifacts.is_empty() {
        lines.push(Line::styled("artifacts", theme.dim_style()));
        for artifact in &extras.artifacts {
            let mut spans = vec![
                Span::styled("  ", theme.base()),
                Span::styled(artifact.name.clone(), Style::new().fg(theme.fg)),
                Span::styled(
                    format!("  {}", human_size(artifact.size_bytes)),
                    theme.dim_style(),
                ),
            ];
            if artifact.expired {
                spans.push(Span::styled("  (expired)", Style::new().fg(theme.dim)));
            }
            lines.push(Line::from(spans));
        }
    }
    if !extras.annotations.is_empty() {
        lines.push(Line::styled("annotations", theme.dim_style()));
        for annotation in &extras.annotations {
            let (glyph, color) = match annotation.level {
                AnnotationLevel::Failure => ("✗", theme.error_fg),
                AnnotationLevel::Warning => ("⚠", theme.warn_fg),
                AnnotationLevel::Notice => ("•", theme.accent),
            };
            let mut spans = vec![Span::styled(format!("  {glyph} "), Style::new().fg(color))];
            let loc = annotation_location(annotation);
            if !loc.is_empty() {
                spans.push(Span::styled(format!("{loc}  "), theme.dim_style()));
            }
            spans.push(Span::styled(
                annotation_text(annotation),
                Style::new().fg(theme.fg),
            ));
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn annotation_location(annotation: &crate::ci::Annotation) -> String {
    match (annotation.path.as_str(), annotation.start_line) {
        ("", _) => String::new(),
        (path, Some(line)) => format!("{path}:{line}"),
        (path, None) => path.to_owned(),
    }
}

fn annotation_text(annotation: &crate::ci::Annotation) -> String {
    let body = if annotation.message.is_empty() {
        annotation.title.clone()
    } else {
        annotation.message.clone()
    };
    body.lines().next().unwrap_or_default().to_owned()
}

/// Bytes as a compact `1.2 KB` / `3.4 MB`, matching how forges list artifacts.
// the f64 cast only feeds a one-decimal display, so mantissa loss is moot
#[allow(clippy::cast_precision_loss)]
fn human_size(bytes: u64) -> String {
    let mut size = bytes as f64;
    let mut units = ["B", "KB", "MB", "GB"].into_iter();
    let mut unit = units.next().unwrap_or("B");
    while size >= 1024.0 {
        let Some(next) = units.next() else { break };
        size /= 1024.0;
        unit = next;
    }
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::{Annotation, Artifact};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::App;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::standard_fixture;

    fn graph_app_with_extras(extras: RunExtras) -> App {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.graph = Some(crate::graph::GraphView::new());
        app.handle(AppEvent::CiExtras(extras));
        app
    }

    #[test]
    fn human_size_scales_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn no_panel_without_extras_keeps_full_body() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.graph = Some(crate::graph::GraphView::new());
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        let rendered = format!("{:?}", terminal.backend());
        assert!(
            !rendered.contains("run extras"),
            "no extras → no panel carved from the graph body"
        );
    }

    #[test]
    fn renders_artifacts_and_annotations_panel() {
        let extras = RunExtras {
            artifacts: vec![Artifact {
                name: "coverage".into(),
                size_bytes: 2048,
                expired: false,
            }],
            annotations: vec![Annotation {
                level: AnnotationLevel::Warning,
                title: "clippy".into(),
                message: "unused import".into(),
                path: "src/lib.rs".into(),
                start_line: Some(12),
            }],
        };
        let mut app = graph_app_with_extras(extras);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
