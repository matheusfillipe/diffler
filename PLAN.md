# Roadmap

Current state, architecture, and distribution: see `AGENTS.md`.
M1 (the review loop) shipped and is released across all channels.

## Next

- **M2 — sources + ergonomics**: commit/range diff sources, side-by-side
  toggle, themes, stash/push/pull popups, agent-trigger hook polish. *(Landed:
  commit/range sources + per-source review state, file-sidebar diff view,
  stash + network popups, side-by-side toggle, themes.)* Dropped: worktree
  listing and patch-file source.
- **M3 — workload tracking**: task model, home-screen board, task MCP tools.
- **M4 — LSP / blast radius**: minimal JSON-RPC client pool over stdio,
  Helix-style server registry (PATH probe + install hints, config override),
  changed-symbol impact via `documentSymbol` + `references` — file-header
  impact badge, symbol impact popup, later the impact graph and
  sort-by-impact sidebar.

Later: jj VCS backend, structural (difftastic-style) diff toggle.
