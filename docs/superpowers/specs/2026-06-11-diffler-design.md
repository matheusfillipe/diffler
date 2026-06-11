# Diffler — Design Spec

Date: 2026-06-11. Status: draft for review.

## What

Terminal code review companion for AI coding agents. Launched in a repo (typically before/alongside
Claude Code), it renders a live-updating, magit-style review UI and embeds an MCP server so any
MCP-compatible harness interacts with the review: reads comments, receives hunk verdicts, reports
work. Human reviews; agent responds; diff updates on the fly.

Philosophy: YAGNI, KISS, suckless. One small native binary, alternate-screen TUI, dies with the
terminal, no daemon, no browser.

## Why (gap, validated in research/)

Nobody combines: (1) MCP as the review protocol in a terminal TUI, (2) hunk-level accept/reject in
an agent loop, (3) review + workload tracking in one surface, (4) live bidirectional updates.
Closest competitors: hunk (custom daemon, TS, no verdicts), revdiff (one-shot exit codes), crit
(web), tuicr (no MCP — rejected upstream), vibe-kanban (web, sunsetting). See `research/LANDSCAPE.md`.

## Stack (locked — see research/STACK.md, research/VALIDATION.md)

Rust (edition 2024) · ratatui 0.30 + crossterm 0.29 · tokio · rmcp 1.7 (streamable HTTP via axum,
localhost) · git2 0.20 · async-lsp 0.2 · notify 8.2 + debouncer · similar (histogram + inline) ·
syntect + two-face. Decided after 4 side-by-side demos (`demos/`); deciding factors: agent
self-testability (TestBackend + insta deterministic render tests), native latency, small binary.

## Architecture (single binary, library-core)

Cargo workspace:

```
diffler-core/   pure logic, no terminal: session model, diff pipeline, git, tasks, persistence
diffler/        binary: TUI (ratatui) + MCP server (rmcp/axum) + LSP pool + watcher
```

Runtime (one tokio runtime, everything is tasks + channels):

```
#[tokio::main]
 ├─ MCP server task     axum on 127.0.0.1:{port}/mcp (port printed at startup; --port flag)
 ├─ watcher task        notify → debounce ~200ms → "dirty" signal; 5s fallback tick
 ├─ diff task           dirty → recompute diff + highlight off-thread → swap Arc<DiffModel>
 ├─ lsp pool task       async-lsp clients, lazy per language, registry TOML
 └─ main task           ratatui loop: EventStream + tokio::select! over all channels
```

Rules: render loop never computes (slices visible lines from precomputed model); scroll anchored to
(file, hunk, line-in-hunk) so agent edits don't jump the view; cache HighlightLines per file.

## Core model (diffler-core)

```
Session {
  id, repo_path, diff_source: WorkingTree | RefRange(a, b) | Commit(sha) | Patch(path),
  files: [FileReview { path, hunks: [HunkReview { header, verdict: Pending|Accepted|Rejected,
                                                  lines, comments }],
                       file_comments }],
  comments: Comment { id, author (human|agent name), anchor (file, side, line | hunk | file),
                      body, status: Open|Replied|Resolved, thread: [Reply] },
  tasks: [Task { id, title, status: Todo|InProgress|Review|Done, branch?, attempts: u32, notes }],
}
```

- Verdicts are diffler's differentiator: per-hunk Accept/Reject/Pending, set by human keys (`a`/`x`),
  read by agent via MCP. Rejected hunks carry optional reason (comment-on-hunk).
- Comment status lifecycle (open → replied → resolved) borrowed from diffx; unresolved comments +
  rejected hunks = "not approved".
- Persistence: JSON files under `.diffler/` in the repo (gitignored by default) — session survives
  TUI restarts; agent loses nothing when TUI is down (MCP calls fail → harness retries; state on
  disk). Schema is a semantic superset of tuicr sessions; `diffler export --tuicr` later (v1.5).
- Hunk identity across live updates: content-hash anchored (like tuicr's reviewed_hunks); verdict
  survives unrelated edits, resets if the hunk's content changes (a rejected hunk the agent rewrote
  is genuinely new — back to Pending).

## Diff pipeline (research/DIFF-RENDERING.md recipe, Tier 1)

git2 hunks with histogram algorithm → similarity line-pairing → `similar` char-level intra-line
spans → syntect highlights WHOLE file each side, sliced onto diff lines (correct across hunk
boundaries) → composite: syntax fg, diff line bg, brighter intra-line bg. GitHub-dark theme from
`demos/SPEC.md` is the default; theme = TOML. Progressive: plain text first frame, highlighted swap
when ready.

## Diff sources (no worktree management — research/WORKSPACES.md)

`diffler` (working tree vs HEAD, default) · `diffler main` / `diffler a..b` / `diffler <sha>` ·
`diffler --patch f.patch`. Existing worktrees listed read-only on Home as switchable diff sources.
Diffler never creates/manages isolation — that's the orchestrator's job.

## TUI

Screens:
1. **Home (magit-style)**: sections (TAB fold): Workspaces/branches (read-only diff sources),
   Changes (files +n −n), Tasks (workload board: todo/in-progress/review/done with attempt counts),
   Recent commits. `Enter` opens diff; `j/k` move; section keys jump.
2. **Review (diff view)**: file sidebar + unified diff (side-by-side later), inline comment threads,
   verdict chips per hunk. Keys: `j/k` lines, `J/K` hunks, `]f [f` files, `c` comment, `a` accept
   hunk, `x` reject hunk, `r` reply/resolve, `K`(hover) `gd`(definition) `gr`(references) via LSP,
   `e` open $EDITOR at line, `q` back.
3. **Modals**: comment input, LSP results list (pick → jump or open editor), help (`?`).

Mouse: click select/focus, wheel scroll. Select-to-copy: Shift+drag (documented); OSC52 yank on `y`
for current line/hunk. Status bar: mode chip, repo@branch, MCP port + connected-client indicator,
pending-comment count.

## MCP interface (v1 tools)

```
review_status()                 → { session, files, verdict counts, unresolved comments } — entry point
get_diff(file?)                 → unified diff + hunk ids + verdicts
get_comments(status?)           → comments with anchors + threads   (agent polls this)
reply_comment(id, body)         → posts reply, marks Replied; TUI shows it live
resolve_comment(id)             → agent claims fixed; human confirms resolve in TUI
get_verdicts()                  → hunk verdicts + reject reasons    (agent's fix list)
request_review(summary?)        → BLOCKS until human acts (approve session / comments+rejections
                                  exist) → returns verdict bundle. The human-in-the-loop gate;
                                  long-poll with timeout + cursor so harness timeouts are safe.
list_tasks() / upsert_task(...) → workload tracking; agent reports todo/in-progress/review/done
navigate(file, line)            → TUI jumps (human attention pointer)
```

Notifications: `tools/list_changed` on capability changes; otherwise agents poll (Claude Code lacks
`resources/subscribe`). No sampling (unsupported + deprecated). Elicitation not used in v1 (verdicts
belong in diffler's UI, not harness dialogs).

Connection: TUI prints `claude mcp add --transport http diffler http://127.0.0.1:{port}/mcp` at
startup; `diffler mcp-add` helper runs it. Fixed default port (config) so the registration survives
restarts.

## LSP (registry pattern — research/LSP-CLIENT-ARCHITECTURE.md, via async-lsp)

Bundled `languages.toml` (helix-style: file-types, root markers, server priority chains, install
hints). Detect → first-on-PATH wins → lazy spawn per language → prompt with install command if
missing (never auto-install). Features: hover, definition, references, diagnostics in gutter.
Agent edits → full-text didChange (open files) + didChangeWatchedFiles (wired to notify watcher).
Negotiate utf-8 positions; answer server→client requests; cancel on cursor move; "indexing…" state
from $/progress. LSP failures degrade silently to plain review — never block the review loop.

## Error handling

- TUI closed ⇒ MCP down: by design (no daemon). State persisted in `.diffler/`; agent's tool call
  fails with connection refused; harness retries; human relaunches. `request_review` long-poll
  returns cursor tokens so re-calls resume cleanly.
- Repo edge cases: binary files (marker, no diff), renames (git2 detection), merge conflicts (show
  conflict markers as-is), empty diff (clean-tree screen).
- Watcher missed events: 5s fallback tick guarantees eventual consistency.
- LSP server crash: restart once, then disable for session with status-bar notice.
- Panic safety: ratatui restore hook (no wrecked terminals).

## Testing (3 layers, all agent-drivable)

1. `cargo test`: diffler-core pure-logic tests + ratatui TestBackend + insta snapshots (cell-exact
   UI regression, deterministic).
2. PTY integration: pexpect + pyte harness (proven in demos — `verify.py` pattern), tape-style
   scripts in `tests/tapes/`.
3. MCP E2E: test client calls tools against a running instance, asserts TUI state via PTY scrape.

## Milestones (full vision = v1; shipped incrementally)

- **M1 — review loop**: working-tree diff, live watcher, GitHub-dark rendering (full pipeline),
  comments + verdicts, MCP server with all review tools, persistence. *Useful + differentiated alone.*
- **M2 — sources + ergonomics**: ref/range/patch sources, worktree listing, $EDITOR escape, OSC52,
  side-by-side toggle, themes, mouse polish.
- **M3 — workload tracking**: tasks model + Home board + task MCP tools.
- **M4 — LSP**: pool, registry, hover/def/refs, diagnostics gutter.

## Non-goals

Creating/managing worktrees, containers, branches · forge integration (GitHub/GitLab PRs) ·
difftastic-style structural diff (maybe later as toggle) · being an orchestrator · sampling/elicitation
· Windows (v1 targets macOS/Linux; nothing should preclude later support).

## Open questions

- Crate name availability (`diffler` on crates.io) — check before publish.
- `request_review` long-poll duration vs harness tool-timeout defaults — needs empirical tuning (M1).
- Verdict granularity below hunk (line ranges) — punt unless review demands it.
