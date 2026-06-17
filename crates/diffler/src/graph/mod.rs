//! Host-side glue for the `diffler-graph` component: discover/load a GitHub
//! Actions workflow into a `Model`, re-poll it for live status, and map the app
//! theme onto the component palette. The component (`diffler_graph::GraphView`)
//! does the rendering and interaction; the app owns the event loop, drives the
//! poll, and reacts to the actions.

mod github;

use std::path::{Path, PathBuf};

use diffler_graph::{GraphTheme, Model};

use crate::theme::Theme;

/// A workflow run that can be re-polled for live status.
#[derive(Debug, Clone)]
pub struct GraphPoll {
    yaml: String,
    pub run_id: String,
}

/// Map the app theme onto the component's palette.
pub fn graph_theme(theme: &Theme) -> GraphTheme {
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

/// The first workflow under `.github/workflows/` (preferring `release.yml`).
pub fn discover_workflow(repo_root: &Path) -> Option<PathBuf> {
    let dir = repo_root.join(".github/workflows");
    let preferred = dir.join("release.yml");
    if preferred.is_file() {
        return Some(preferred);
    }
    std::fs::read_dir(&dir).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        let ext = path.extension()?.to_str()?;
        (ext == "yml" || ext == "yaml").then_some(path)
    })
}

/// Load a workflow into a model: the DAG from its `needs`, live status
/// (best-effort) from the latest run. Returns a [`GraphPoll`] when a run id is
/// known so the app can keep watching it.
pub fn load(workflow: &Path) -> color_eyre::Result<(Model, Option<GraphPoll>)> {
    use color_eyre::eyre::WrapErr;
    let yaml = std::fs::read_to_string(workflow)
        .wrap_err_with(|| format!("read workflow {}", workflow.display()))?;
    let run_id = github::latest_run(workflow).ok();
    let jobs = run_id
        .as_deref()
        .map(github::fetch_jobs)
        .and_then(Result::ok)
        .unwrap_or_default();
    let model = github::build_model(&yaml, &jobs)?;
    let poll = run_id.map(|run_id| GraphPoll {
        yaml: yaml.clone(),
        run_id,
    });
    Ok((model, poll))
}

/// Re-poll a watched run and rebuild the model (blocking `gh`; run off the event
/// loop). `None` if the fetch or parse fails — the last good model stays.
pub fn refetch(poll: &GraphPoll) -> Option<Model> {
    let jobs = github::fetch_jobs(&poll.run_id).ok()?;
    github::build_model(&poll.yaml, &jobs).ok()
}
