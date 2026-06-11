# Plan

Design spec: `docs/superpowers/specs/2026-06-11-diffler-design.md`. Research: `research/`.

## Status

Scaffold. Workspace, CI, release pipeline, lints, hooks in place. First core
primitives (`diffler-core::diff::intraline`, `repo::discover`) landed as the
testing pattern reference; everything else starts at M1.

## Milestones

- **M1 — review loop**: working-tree diff, live file watcher, GitHub-dark rendering
  (histogram hunks, intra-line char emphasis, full-file syntax highlight sliced onto
  diff lines), comments + per-hunk accept/reject verdicts, embedded MCP server
  (streamable HTTP) with review tools, session persistence in `.diffler/`.
- **M2 — sources + ergonomics**: ref/range/patch diff sources, worktree listing
  (read-only), `$EDITOR` escape, OSC52 yank, side-by-side toggle, themes.
- **M3 — workload tracking**: task model, home-screen board, task MCP tools.
- **M4 — LSP**: async-lsp client pool, language registry, hover/definition/references,
  diagnostics gutter.

## Distribution (prepared, not active)

- GitHub releases: tag-triggered, native runners, linux x86_64/arm64 (musl),
  macOS arm64/x86_64, windows x86_64.
- crates.io: trusted publishing (OIDC) once the crate is first published manually.
- npm: esbuild-style per-platform optionalDependencies packages, repacking the
  release archives. Names to reserve: `diffler`, `@diffler/*`.
