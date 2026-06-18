//! Host-side glue for the `diffler-graph` component: map the app theme onto the
//! component palette, and discover the repo's workflow file. CI data acquisition
//! (runs, jobs, logs) lives in `diffler-ci` and the `ci` module; this module is
//! just the rendering-side bridge.

use std::path::{Path, PathBuf};

use diffler_graph::GraphTheme;

use crate::theme::Theme;

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
