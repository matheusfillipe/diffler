# Plan

Design spec: `docs/superpowers/specs/2026-06-11-diffler-design.md`. Research: `research/`.

## Status

M1 implemented on `feat/m1`. Landed: diffler-core engine (model, histogram
diff with intra-line emphasis, `Vcs` trait + git2 backend with stage/unstage/
discard/commit/branch, session with comments + viewed marks, `.diffler/` store,
syntect highlighting, markdown feedback export, `Review` facade); TUI (layered
TOML config + `config --dump`, neogit-style status/log/diff screens, visual-mode
comments, viewed marks, OSC52 yank, `$EDITOR` suspend/restore, commit + branch
flows, live watcher); embedded MCP server (streamable HTTP, review tools,
`wait_for_feedback` long-poll); fixture + snapshot + MCP + PTY e2e test layers;
publish workflows (crates.io + npm) and npm scaffold.

## Milestones

- **M1 — review loop** (done, `feat/m1`): neogit-core TUI (status/log/diff,
  stage/unstage/discard/commit/branch), comments + visual-mode comments +
  viewed marks + markdown export, live watcher, embedded MCP server with the
  review tool set, fixture+PTY+MCP test layers, publish workflows, README.
- **M2 — sources + ergonomics**: ref/range/patch diff sources, worktree listing
  (read-only), side-by-side toggle, themes, stash/push/pull popups,
  agent-trigger hook polish, first crates.io + npm release.
- **M3 — workload tracking**: task model, home-screen board, task MCP tools.
- **M4 — LSP**: async-lsp client pool, language registry, hover/definition/references,
  diagnostics gutter.

## Distribution

- GitHub releases: tag-triggered, native runners, linux x86_64/arm64 (musl),
  macOS arm64/x86_64, windows x86_64/arm64. In place.
- Publish workflow (`publish.yml`): runs on release published; crates.io via
  `CARGO_REGISTRY_TOKEN`, npm repack via `scripts/npm-pack.sh` + `NPM_TOKEN`
  (esbuild-style per-platform `optionalDependencies`, entry shim in `npm/`).
  Dry-runnable via workflow_dispatch; no-ops without secrets.
- First crates.io publish must be manual (then trusted publishing can be
  configured). npm names to reserve: `diffler`, `@diffler/*`.
