//! Host-side glue for the `diffler-graph` component: map the app theme onto the
//! component palette. CI data acquisition (runs, jobs, logs) lives in
//! `diffler-ci` and the `ci` module; this module is just the rendering bridge.

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
