# Research Validation — 2026-06-11

Claim-by-claim validation of the June 2 research docs (`RESEARCH.md`, `LSP-CLIENT-ARCHITECTURE.md`,
`TUI-AGENT-TESTING.md`), via live deep research. Originals kept as-is; this doc supersedes where they conflict.

## Verdict summary

| Prior decision | Verdict | Correction |
|---|---|---|
| Rust + ratatui + crossterm | **CONFIRMED** | ratatui 0.30.1 (Jun 2026) + crossterm 0.29 is current mainstream. |
| Fork/base on tuicr | **REVERSED** | Build from scratch; interop with tuicr's session JSON instead. See below. |
| gix for git ops | **REVERSED** | git2 0.20. gix 0.84 lacks `worktree add/remove`; tuicr and gitui use git2 (gitui mid-migration). |
| tree-sitter structural diff in core | **DEFERRED** | YAGNI. git-lib hunks + `similar` crate for intra-line word diff. tree-sitter only if structural diff becomes a feature. |
| Custom ~500-line LSP client | **REVERSED** | `async-lsp` 0.2.4 (oxalica) is a maintained, client-capable crate. Its 171-line example covers ~80% of our needs. |
| tower-lsp for LSP | **CONFIRMED-NEGATIVE** | tower-lsp server-only AND stale since 2023; irrelevant either way. |
| rmcp for MCP server | **CONFIRMED, now verified** | rmcp 1.7.0, post-1.0, ~biweekly releases, 12M+ downloads, used by OpenAI Codex. Streamable HTTP server is first-class (axum-native). |
| pexpect + pyte agent testing | **CONFIRMED** | Pipeline unchanged and proven. Plus ratatui `TestBackend` + insta for unit layer. |
| syntect for highlighting | **CONFIRMED, refined** | syntect 5.2 + `two-face` 0.5 (maintained syntax/theme bundle; avoids Oniguruma C dep via fancy-regex). tuicr-validated pairing. |

## tuicr: fork → no (detailed)

State as of 2026-06-11: 897★, MIT, v0.17.1, **60,350 lines** (prior doc said 37.3k — grew 60%),
~30 commits / 2 weeks. Claims about architecture (App state, VcsBackend git/jj/hg, ForgeBackend,
ReviewStore lib API, skills bundle, JSON CLI) all verified. New since prior doc: GitLab forge,
hunk-level reviewed state (PR #350).

Why not fork:
- **MCP rejected upstream**: issue #310 contains "Rejected Alternative: MCP Server In Core" —
  deliberate maintainer stance, not an omission. Our core feature direction is declined upstream.
- **Sync runtime**: no tokio, `ureq` for HTTP. Embedded HTTP MCP server + LSP pool need async —
  structural graft, not a patch.
- **No verdict concept**: tuicr's `reviewed` is boolean seen-state + `reviewed_hunks` set; comment
  types are `issue|suggestion|note|praise`. Accept/reject verdicts = schema extension.
- **No worktree support, currently broken in worktrees** (issue #405).
- Fast-moving 60k-line upstream + structural delta = permanent divergent fork.

What we take instead:
- **Interop**: read/write tuicr session JSON (self-describing, lock-protected, documented below) so
  diffler comments are visible to tuicr's agent-skill ecosystem. Optionally depend on `tuicr` crate's
  `ReviewStore` API.
- **Vendor selectively** (MIT): `src/vcs/` backends and `src/ui/diff_*` rendering are highest-value.

### tuicr on-disk format (for interop)

- Dir: `~/Library/Application Support/tuicr/reviews/` (macOS), `~/.local/share/tuicr/reviews/` (Linux)
- `index.json` — manifest v2.0, slug → metadata; rebuildable from sessions.
- `sessions/<16-hex>.json` — `ReviewSession`: id, repo_path, branch, base_commit, diff_source,
  review_comments[], per-file `FileReview { path, reviewed, file_comments, line_comments,
  reviewed_hunks, content_hash }`.
- `active_sessions.json` — live TUI registry (pid, slug, last-seen) — how agents find open sessions.

## LSP: use async-lsp (detailed)

- `async-lsp` 0.2.4 (2026-04-24, oxalica, 829K downloads): tower-based, explicitly symmetric
  client/server, `MainLoop::new_client`, tokio feature, stdio. Re-exports lsp-types **0.95** (pinned —
  fine: call hierarchy is LSP 3.16, present in 0.95).
- `lsp-types` upstream dormant since 2024-06 (0.97); helix and zed both vendor forks. Version dictated
  by async-lsp.
- Start from `examples/client_builder.rs` (171 lines): spawn server, initialize, didOpen, wait for
  `$/progress` indexing-done, hover with `ContentModified` retry, diagnostics routing.
- Realistic client logic for our feature set: 300–500 lines on async-lsp vs 700–1200 robust DIY.
- Registry pattern from `LSP-CLIENT-ARCHITECTURE.md` (helix languages.toml style, detect+prompt,
  fallback chains) remains valid and is still the right design.

### Gotchas verified (matter for agent-edits-while-human-reviews)

- Once a file is `didOpen`ed, **client owns truth** — server ignores disk. When agent edits an open
  file: full-text `didChange` (TextDocumentSyncKind::FULL) with bumped version. Simplest correct.
- Non-open files agent touched: honor `workspace/didChangeWatchedFiles` dynamic registration (wire
  our notify watcher to it), else rust-analyzer goes stale.
- Negotiate `positionEncodings: ["utf-8","utf-16"]`; default is UTF-16 code units — off-by-N bugs on
  non-ASCII if ignored.
- Must answer server→client requests (`client/registerCapability`, `workspace/configuration`,
  `window/workDoneProgress/create`) or some servers stall. async-lsp's Router forces this.
- Cancel in-flight hover/definition on cursor move (`$/cancelRequest`).

## MCP: rmcp verified (detailed)

- rmcp 1.7.0 (2026-05-13), targets spec **2025-11-25** (current). Post-1.0 semver, biweekly minors.
- Streamable HTTP server: `StreamableHttpService` implements tower `Service` → nests in axum
  `Router`. `LocalSessionManager` for localhost single-process. `CancellationToken` for shutdown.
- Claude Code: `claude mcp add --transport http diffler http://localhost:PORT/mcp` — exactly
  supported. SSE deprecated ecosystem-wide.
- Claude Code honors `tools/list_changed` notifications + auto-reconnects HTTP (5 attempts, backoff).
  **`resources/subscribe` NOT supported by Claude Code** — don't build live-update on it; use
  tool-call polling + `notify_tool_list_changed`.
- **Elicitation supported end-to-end** (Claude Code renders dialogs; rmcp `elicit::<T>()`). But it
  renders in *Claude Code's* terminal — for "approve in diffler UI" semantics, block the tool call on
  a channel the ratatui loop resolves.
- **Sampling: avoid** — unsupported in Claude Code, deprecated in 2026-07-28 spec RC.
- Spec 2026-07-28 RC: stateless core (drops initialize handshake/session ids), sampling deprecated.
  Localhost design unaffected; expect rmcp minor bump H2 2026.
- Runtime coexistence: single `#[tokio::main]`; `tokio::spawn(axum::serve(...))`; ratatui loop via
  crossterm `EventStream` + `tokio::select!`. Never block a runtime thread on sync `event::poll`.
- Prior art: `nereid` (ratatui + embedded MCP, shared live diagram), `tui-mcp`, `md-redline`
  (MCP review surface for markdown). No terminal TUI does MCP code review — open niche.

## Git + live updates (detailed)

- **git2 0.20** for everything: diff, rename detection, status, worktree add/remove/list. Proven by
  tuicr + gitui. gix 0.84 read-path is viable (status/diff/worktree-list done) but worktree mutation
  missing — would force shelling out anyway.
- **notify 8.2 + notify-debouncer-full 0.7** (9.0 RC out, pin 8.2). Watch worktree root +
  `.git/{HEAD,refs,packed-refs}`. Debounce ~200ms. Filter `.git/index.lock`, swap files, own writes.
  Plus 5–10s fallback tick (gitui demoted notify from default due to platform flakiness; lazygit
  polls 10s — both retreated to polling for reliability).
- macOS FSEvents coalesces kinds; treat any event as "dirty", don't pattern-match kinds. Linux
  inotify: atomic saves arrive as Create+Rename.
- **Diff pipeline**: background tokio task recomputes diff + highlights → swap `Arc<DiffModel>` →
  redraw. Never compute in render loop.
- **Intra-line word diff**: `similar` 3.1.1 `iter_inline_changes()` on changed line pairs (only crate
  with this out of the box). imara-diff only if profiling demands.
- **Flicker**: ratatui double-buffers and cell-diffs — flicker only from `Terminal::clear()` per
  frame or blocked render loop. Virtualize: slice visible lines from precomputed buffer; never giant
  `Paragraph::scroll()` (reprocesses whole text per frame, ratatui #2342). Anchor scroll to
  (file, hunk, line-in-hunk), not absolute row, so agent edits above cursor don't jump the view.
- Cache `HighlightLines` per file, invalidate on change — highlighting is the expensive part.

## Sources

Full source URLs preserved in agent reports; key ones:
- tuicr: github.com/agavra/tuicr · issues #310 (MCP rejection), #405 (worktrees) · docs/REVIEW_CLI.md
- rmcp: github.com/modelcontextprotocol/rust-sdk · docs.rs/rmcp · blog.modelcontextprotocol.io (2026-07-28 RC)
- Claude Code MCP: code.claude.com/docs/en/mcp
- async-lsp: github.com/oxalica/async-lsp (examples/client_builder.rs)
- gitoxide status: github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md · gitui #2676
- ratatui: ratatui.rs/tutorials/counter-async-app/async-event-stream · discussion #1880, issue #2342
- notify: docs.rs/notify-debouncer-full · gitui FAQ · lazygit #5278
