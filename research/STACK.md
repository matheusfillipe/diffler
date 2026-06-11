# Diffler — Validated Stack (2026-06-11)

Final stack after validation pass. Supersedes the table in `RESEARCH.md`.

| Component | Choice | Version | Notes |
|---|---|---|---|
| Language | Rust | edition 2024 | |
| TUI | ratatui + crossterm | 0.30.1 / 0.29 | alternate screen via `ratatui::init()/restore()`; crossterm `event-stream` feature |
| Async runtime | tokio | 1.x | single `#[tokio::main]`; everything is tasks + channels |
| Git | git2 | 0.20 | diff, rename detection, status, worktree add/remove/list. NOT gix (no worktree mutation), NOT shelling out for diffs |
| MCP server | rmcp | 1.7+ | `transport-streamable-http-server` + session features; axum `Router::nest_service("/mcp", ...)`; `LocalSessionManager`; localhost only |
| HTTP | axum | (rmcp-compatible) | hosts the MCP endpoint inside the TUI process |
| LSP client | async-lsp | 0.2.4 | tokio feature; lsp-types 0.95 re-exported; start from examples/client_builder.rs |
| File watching | notify + notify-debouncer-full | 8.2 / 0.7 | ~200ms debounce + 5–10s fallback tick |
| Line diff | git2 hunks | — | git-semantics-correct (renames, binary, filters) |
| Intra-line diff | similar | 3.1.1 | `iter_inline_changes()` for word-level highlighting |
| Highlighting | syntect + two-face | 5.2 / 0.5 | `syntect-default-fancy` (no Oniguruma C dep); cache `HighlightLines` per file |
| Testing L1 | ratatui TestBackend + insta | — | unit/render snapshots |
| Testing L2/L3 | pexpect + pyte (+ tape DSL) | — | PTY-driven agent tests, headless CI |

## Architecture skeleton

```
#[tokio::main]
 ├─ tokio::spawn(axum::serve(...))      MCP server, 127.0.0.1:{port}/mcp
 │    └─ tool handlers hold channel senders (Arc-shared core state)
 ├─ tokio::spawn(watcher task)          notify → debounce → "repo dirty" mpsc
 ├─ tokio::spawn(diff task)             dirty → recompute diff + highlight → Arc<DiffModel> swap
 ├─ tokio::spawn(lsp pool)              async-lsp clients, lazy per language, registry pattern
 └─ main task: ratatui loop             EventStream + tokio::select! over {keys, ticks, models, mcp requests}
```

Rules:
- Render loop never computes; only slices visible lines from precomputed model (no giant
  `Paragraph::scroll`).
- Scroll anchored to (file, hunk, line-in-hunk) — survives agent edits above cursor.
- Human-in-the-loop gate: MCP tool call blocks on oneshot resolved by TUI keypress
  (verdict in OUR ui). rmcp elicitation only for prompts meant for the harness's terminal.
- Live update to agent: `notify_tool_list_changed` + tool polling. No `resources/subscribe`
  (Claude Code doesn't support it).
- Agent edits open file → full-text `didChange` with bumped version; non-open files →
  `workspace/didChangeWatchedFiles` wired to our notify watcher.

## Interop

- Read/write tuicr session JSON (`~/.local/share/tuicr/reviews/sessions/*.json`) so tuicr's agent
  skill ecosystem sees diffler comments. Diffler's own schema adds verdicts (accept/reject per hunk)
  — superset, not mapping (tuicr has no verdict concept).

## Connection (user side)

```bash
diffler                      # starts TUI + MCP server, prints port
claude mcp add --transport http diffler http://localhost:PORT/mcp
```
