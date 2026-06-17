//! Host-side glue for the `diffler-graph` component: a GitHub Actions source,
//! a theme converter, and a small run loop that owns the terminal, events, and
//! the live `gh` poll. The component (`diffler_graph::GraphView`) does the
//! rendering and interaction; this module supplies it state and reacts to its
//! actions. The in-app view-stack embedding lands with the window manager.

mod github;

use std::path::Path;

use crossterm::event::KeyCode;
use diffler_graph::{GraphAction, GraphTheme, GraphView, Model};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as LayoutArea};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::event::{self, AppEvent};
use crate::theme::Theme;

pub use diffler_graph::Model as GraphModel;

/// A workflow run that can be re-polled for live status.
pub struct LiveSource {
    yaml: String,
    run_id: String,
}

/// Map the app theme onto the component's palette.
fn graph_theme(theme: &Theme) -> GraphTheme {
    GraphTheme {
        bg: theme.bg,
        fg: theme.fg,
        dim: theme.dim,
        ok: theme.added,
        failed: theme.error_fg,
        running: theme.warn_fg,
        queued: theme.dim,
        panel: theme.panel,
    }
}

/// Build a model from a GitHub Actions workflow: the DAG from its `needs`, live
/// status (best-effort) from the latest run or `run`. Returns a [`LiveSource`]
/// when a run id is known so the screen can watch it.
pub fn github_source(
    workflow: &Path,
    run: Option<String>,
) -> color_eyre::Result<(Model, Option<LiveSource>)> {
    use color_eyre::eyre::WrapErr;
    let yaml = std::fs::read_to_string(workflow)
        .wrap_err_with(|| format!("read workflow {}", workflow.display()))?;
    let run_id = run.or_else(|| github::latest_run(workflow).ok());
    let jobs = run_id
        .as_deref()
        .map(github::fetch_jobs)
        .and_then(Result::ok)
        .unwrap_or_default();
    let model = github::build_model(&yaml, &jobs)?;
    let live = run_id.map(|run_id| LiveSource {
        yaml: yaml.clone(),
        run_id,
    });
    Ok((model, live))
}

/// How often to re-poll a watched run.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Run the graph screen to completion. Owns terminal + mouse capture; the
/// component does the rest.
pub async fn run(model: Model, theme: Theme, live: Option<LiveSource>) -> color_eyre::Result<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    let terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let result = run_loop(terminal, model, theme, live).await;
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

async fn run_loop(
    mut terminal: ratatui::DefaultTerminal,
    model: Model,
    theme: Theme,
    live: Option<LiveSource>,
) -> color_eyre::Result<()> {
    let gtheme = graph_theme(&theme);
    let watching = live.is_some();
    let mut view = GraphView::new();
    view.set_model(model);
    let mut message: Option<String> = None;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let _events = event::spawn_event_loop(tx);
    let (status_tx, mut status_rx) = mpsc::unbounded_channel();
    let yaml = live.as_ref().map(|l| l.yaml.clone());
    if let Some(live) = live {
        tokio::spawn(poll_run(live.run_id, status_tx));
    }

    loop {
        terminal.draw(|frame| {
            draw(
                frame,
                &mut view,
                &gtheme,
                &theme,
                watching,
                message.as_deref(),
            );
        })?;
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    AppEvent::Quit => break,
                    AppEvent::Key(key) => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        _ => if let Some(action) = view.on_key(key) {
                            message = act(&action);
                        },
                    },
                    AppEvent::Mouse(mouse) => {
                        if let Some(action) = view.on_mouse(mouse) {
                            message = act(&action);
                        }
                    }
                    _ => {}
                }
            }
            jobs = status_rx.recv() => {
                match jobs {
                    Some(jobs) => {
                        if let Some(yaml) = yaml.as_deref()
                            && let Ok(refreshed) = github::build_model(yaml, &jobs)
                        {
                            view.set_model(refreshed);
                        }
                    }
                    None => break,
                }
            }
        }
    }
    Ok(())
}

/// Turn a component action into a one-line status message (the placeholder for
/// real per-node effects like opening code or a job log).
fn act(action: &GraphAction) -> Option<String> {
    match action {
        GraphAction::Activated(id) => Some(format!("open {} (not wired yet)", id.0)),
        GraphAction::Folded { .. } => None,
    }
}

fn draw(
    frame: &mut Frame<'_>,
    view: &mut GraphView,
    gtheme: &GraphTheme,
    theme: &Theme,
    watching: bool,
    message: Option<&str>,
) {
    let area = frame.area();
    frame.render_widget(ratatui::widgets::Block::new().style(theme.base()), area);
    let [hint, body, bar] = LayoutArea::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            " hjkl move · n/N edge · ⏎ open/fold · +/- zoom · g/G ends · q quit",
            Style::new().fg(theme.dim),
        )),
        hint,
    );
    view.render(body, frame.buffer_mut(), gtheme);
    let watch = if watching { "  ⟳ watching" } else { "" };
    let status = message.map_or_else(
        || format!(" GRAPH  zoom: {}{watch}", view.zoom().label()),
        |m| format!(" GRAPH  {m}"),
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            status,
            Style::new().fg(theme.fg).bg(theme.panel),
        ))
        .style(Style::new().bg(theme.panel)),
        bar,
    );
}

/// Poll a run's job statuses forever, emitting each fetch on `tx`.
async fn poll_run(run_id: String, tx: mpsc::UnboundedSender<Vec<github::JobStatus>>) {
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    loop {
        ticker.tick().await;
        let id = run_id.clone();
        let fetched = tokio::task::spawn_blocking(move || github::fetch_jobs(&id)).await;
        if let Ok(Ok(jobs)) = fetched
            && tx.send(jobs).is_err()
        {
            break;
        }
    }
}
