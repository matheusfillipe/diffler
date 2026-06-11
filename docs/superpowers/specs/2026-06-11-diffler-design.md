# Diffler — Design Spec

Date: 2026-06-11 (revised same day: verdicts dropped, neogit UX adopted, git ops in M1).
Status: approved, M1 in progress.

## What

Terminal code review companion for AI coding agents. Launched in a repo (typically alongside
Claude Code), it renders a live-updating, neogit-style git UI and embeds an MCP server so any
MCP-compatible harness reads review comments in place, answers questions, and reacts to
feedback. Human reviews and operates git; agent responds; diff updates on the fly.

Philosophy: YAGNI, KISS, suckless. One small native binary, alternate-screen TUI, dies with
the terminal, no daemon, no browser.

## Why (gap, validated in research/)

Nobody combines: (1) MCP as the review protocol in a terminal TUI, (2) a real magit-class
git UI for the terminal review loop, (3) live bidirectional updates. Closest competitors:
hunk (custom daemon, TS, no git ops), revdiff (one-shot exit codes), crit (web), tuicr
(no MCP — rejected upstream). See `research/LANDSCAPE.md`, `research/NEOGIT-UX.md`.

## Design decisions log

- **Verdicts (hunk accept/reject) dropped.** A bare reject carries no signal the agent can
  act on. Comments are the feedback channel; "mark file viewed" covers the bookkeeping need.
- **Worktrees/workspaces postponed** — undecided what multi-workspace UX should be.
- **LSP postponed** (M3+). Architecture keeps room (async-lsp validated in research).
- **Agent triggering**: MCP servers cannot initiate agent turns (sampling unsupported in
  Claude Code, deprecated in spec 2026-07-28 RC). The loop is: agent long-polls
  `wait_for_feedback`; the human submitting feedback unblocks it. Optionally documented
  Claude Code hook snippet injects pending comments. Diffler spawning `claude -p` itself
  is a possible future feature, not M1.

## Stack (locked — research/STACK.md, research/VALIDATION.md)

Rust edition 2024 · ratatui 0.30 + crossterm 0.29 · tokio · rmcp 1.7 (streamable HTTP via
axum, localhost) · git2 0.21 · notify 8.2 + debouncer · similar 3 (histogram + graphemes) ·
syntect + two-face · serde/serde_json · uuid.

## Architecture (single binary, library-core)

```
diffler-core/   pure logic: VCS trait + git backend, diff model, pairing/emphasis,
                session (comments, viewed marks), markdown export, store, highlight
diffler/        binary: TUI (ratatui) + MCP server (rmcp/axum) + watcher
```

### VCS abstraction

All version-control access goes through a `Vcs` trait in diffler-core (status, diffs, log,
stage/unstage/discard, commit, branches). Only implementation now: git via git2. The trait
exists because jj is coming; no second backend is built or stubbed (YAGNI), but nothing
above the trait may import git2.

### Runtime

One tokio runtime: MCP server task (axum, `127.0.0.1:{port}/mcp`), watcher task (notify →
debounce ~200ms → refresh signal, 5s fallback tick), diff/highlight recompute task (swaps
`Arc<...>` models), main task = ratatui loop (`EventStream` + `select!`). Render loop never
computes; scroll anchored to (file, hunk, line-in-hunk); HighlightLines cached per file.

## Core model

```
Session {
  comments: [Comment { id, author, anchor { file, line, on_old_side, hunk_id, line_text },
                       body, status: Open|Replied|Resolved, replies, at }],
  viewed:   { path -> content_hash },   // GitHub-style; resets when file changes
}
```

- Comment anchors carry a `line_text` snapshot; UI marks comments outdated when the line
  changed. Visual-mode comments anchor to a line range (start..end).
- Viewed marks: `v` toggles; cleared automatically when the file's content hash changes.
- Persistence: JSON in `.diffler/` (atomic temp+rename, self-gitignored). Session survives
  TUI restarts; agent tool calls fail while TUI is down (no daemon, by design) and harnesses
  retry.

## Diff pipeline (research/DIFF-RENDERING.md Tier 1)

git2 hunks → similarity line-pairing → grapheme-level intraline emphasis (byte ranges) →
syntect highlights WHOLE files, sliced onto diff lines → composite syntax-fg over diff-bg
over emphasis-bg. GitHub-dark default theme. Progressive render (plain first frame OK).

## TUI (UX contract: research/NEOGIT-UX.md)

Doom-emacs/neogit keybindings are the default. Screens:

1. **Status (initial view)** — neogit layout: Head line, Untracked / Unstaged / Staged
   sections (files expand inline to diffs via TAB), Recent commits (10, folded).
   Keys: `j/k`, TAB fold, `s/u` stage/unstage (file or hunk), `S/U` all, `x` discard
   (confirmed), `cc` commit ($EDITOR flow, submit/abort), `b` branch popup (`c` create+
   checkout, `n` create, `D` delete), `ll` log, `<cr>` open, `<c-r>` refresh, `{`/`}` hunk
   jumps, `?` help, `q` quit.
2. **Log view (`ll`)** — `[7-char oid] [decorations] [subject]` rows, `<cr>` opens that
   commit's diff in the diff view, j/k scroll, q back.
3. **Diff/review view** — working-tree diff or a commit's diff; magit-style continuous
   scroll across files/hunks; TAB folds files; syntax + word-level emphasis rendering.
   Review keys: `c` comment on cursor line, `V` visual line-select then `c` comment on
   range, `r` reply/resolve thread under cursor, `v` mark file viewed, `y` copy current
   file's feedback as markdown, `Y` copy all feedback as markdown, `e` open `$EDITOR` at
   the cursor's file:line (TUI suspends, subprocess runs, view + cursor restored on exit).
4. **Modals** — comment input, confirm dialogs, branch popup, help. Transient popup model.

Mouse: click select, wheel scroll. OSC52 for clipboard (works over ssh/tmux).

## Feedback markdown export

`y`/`Y` produce markdown for paste into any agent:

```markdown
## Review feedback — <repo> @ <branch> (<n> comments)

### src/auth.py:42 (… context: `if claims.expiry <= now() - LEEWAY:`)
> why LEEWAY here? clock skew? link the incident.

### src/auth.py:50-58 (range)
> this whole block duplicates validate_token
```

Includes file path, line/range, short diff/file context snippet, comment body, thread state.

## MCP interface (v1 tools)

```
review_status()                  → repo, branch, files changed, viewed map, open comment count
get_diff(file?)                  → unified diff text (whole or one file)
get_comments(status?)            → comments with anchors, context snippet, threads
reply_comment(id, body)          → agent answers in place; TUI renders the reply live
resolve_comment(id)              → agent proposes resolved; human confirms in TUI (status
                                   moves to Replied until human resolves — agent cannot
                                   close the loop alone)
wait_for_feedback(timeout_s)     → long-poll; returns when the human posts/edits comments
                                   or presses the "send to agent" key; cursor token for
                                   safe re-polling. THE agent-trigger mechanism.
mark_viewed(file) / viewed map exposure for agent awareness (read-only effect on TUI list)
```

Connection: TUI prints `claude mcp add --transport http diffler http://127.0.0.1:{port}/mcp`
at startup; fixed default port (config). Notifications: tool polling only (Claude Code has
no resources/subscribe). No sampling, no elicitation in v1.

## Testing (3 layers, all agent-drivable)

1. `cargo test` / nextest: core logic against **fixture git repos built in tempdirs**
   (git2-driven `Fixture` helper: write/commit/branch/stage states), ratatui TestBackend +
   insta snapshots for every screen.
2. PTY integration: pexpect + pyte harness driving the real binary inside /tmp fixture
   repos (pattern proven in demos/*/verify.py): launch, assert rendered status, stage,
   comment, copy, quit.
3. MCP E2E: HTTP test client calls tools against a running instance; asserts session state
   and TUI render (via PTY scrape).

## Distribution

- GitHub releases: tag-triggered, 6 native-runner targets (in place).
- crates.io + npm: publish workflows using repo secrets (`CARGO_REGISTRY_TOKEN`,
  `NPM_TOKEN`). npm = esbuild-style per-platform `optionalDependencies` packages +
  `diffler` entry package with launcher shim; repacks release archives.
- README documents: install (cargo/npm/binary), MCP setup for Claude Code, the agent
  feedback loop, hook snippet.

## Milestones

- **M1 (current)**: everything above — neogit-core TUI (status/log/diff, stage/unstage/
  discard/commit/branch), comments + visual-mode comments + viewed marks + markdown export,
  live watcher, MCP server with the tool set, fixture+PTY+MCP test layers, publish
  workflows, README.
- **M2**: ref/range/patch diff sources, $EDITOR jump, side-by-side, themes, stash/push/pull
  popups, agent-trigger hook polish.
- **M3**: workload tracking; LSP. **Later**: jj backend, structural diff toggle.

## Non-goals

Worktree/workspace management (undecided, postponed) · forge (GitHub PR) integration ·
orchestrating agents (spawning `claude -p` is future work) · Windows in v1 (build exists,
unsupported) · difftastic-style structural diff.
