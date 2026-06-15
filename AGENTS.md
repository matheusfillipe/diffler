# diffler — agent guide

Terminal code-review companion for AI agents. Launched in a repo, it renders a
live neogit-style git UI and embeds an MCP server, so an agent reads review
comments in place, replies, and reacts to feedback. The human reviews and drives
git; the agent responds; the diff updates live. Philosophy: YAGNI/KISS — one
small native binary, alternate-screen TUI, no daemon, no browser.

## Layout

```
crates/diffler-core/   pure logic, no terminal (errors via thiserror):
  vcs.rs / git.rs      Vcs trait + git2 backend (status, diff, log, stage, commit, branch)
  model.rs diff.rs     diff model, hunks
  pairing.rs           similarity line-pairing + grapheme intraline emphasis
  highlight.rs         syntect whole-file highlight
  source.rs review.rs  ReviewSource + per-source review state
  session.rs           comments + viewed marks
  store.rs             .diffler/ persistence
  feedback.rs          markdown feedback export

crates/diffler/        binary (errors via color-eyre):
  ui/ app/ tree.rs     ratatui TUI: screens, file sidebar, state
  keymap.rs config.rs  configurable keybindings, layered TOML config
  theme.rs transient.rs  rendering theme, popup/modal model
  mcp.rs               rmcp/axum MCP server
  watch.rs             notify filesystem watcher
  editor.rs clipboard.rs  $EDITOR suspend/restore, OSC52 yank
```

## Commands (just; see `just --list`)

- `just check` — clippy, run after every change
- `just test` — nextest + doctests
- `just fix` — clippy --fix + fmt
- `just snap` — insta snapshot tests; read `.snap.new` diffs before `just snap-accept`
- `just e2e` — PTY end-to-end suite (needs `uv`; CI runs it in a separate job)
- `just ci` — fmt+clippy+tests gate, must pass before any commit (CI additionally runs msrv, deny, typos)

## Rules

- Code is done only when `just ci` passes. Run it, don't assume.
- No `unwrap`/`panic!`/`todo!` in non-test code (clippy denies). `expect` needs justification.
- Errors: `thiserror` in diffler-core; `color-eyre` only in the binary.
- No `println!`/stdout writes in the TUI (corrupts the screen; clippy denies it).
- Async: never block in async fns; `spawn_blocking` for CPU/IO-heavy work.
- TUI changes need TestBackend + insta snapshot coverage. A changed snapshot is a
  behavior change — read the diff, never accept blindly, never edit `.snap` by hand.
- Hooks are managed by prek (`prek install` once). If a hook fails, fix the cause.
  Never `git commit --no-verify`.
- Review before committing: in Claude Code run `/rev` on the working tree for any
  non-trivial change.
- Commit messages: short, imperative, one line. No body unless the why is non-obvious.
- Comments explain why, never what. No change-history commentary.
- New dependencies: add to `[workspace.dependencies]`, justify in the commit.

## Architecture & decisions

- **Layering.** Nothing above the `Vcs` trait may import git2. Only the git2
  backend exists; the trait is there because jj is planned — but no second
  backend is built or stubbed (YAGNI).
- **Runtime.** One tokio runtime: MCP server (axum, `127.0.0.1:{port}/mcp`),
  notify watcher (debounce ~200ms → refresh), diff/highlight recompute (swaps
  `Arc` models), main task = the ratatui loop. The render loop never computes;
  diff pairing and highlighting are deferred to first use.
- **Review state is per diff source.** A `ReviewSource` is `WorkingTree`,
  `Commit{oid}`, or `Range{oldest,newest}`. Comments (anchored to file + line +
  a `line_text` snapshot so stale anchors show as outdated; visual mode anchors a
  range; status Open/Replied/Resolved + threads) and GitHub-style viewed marks
  (keyed by file content hash, auto-cleared on change) are stored **per source**:
  `.diffler/reviews/<key>.json` where key ∈ {`working`, `commit-<oid>`,
  `range-<a>-<b>`}. Legacy `.diffler/session.json` migrates to `reviews/working.json`.
  `.diffler/` self-gitignores. No daemon: agent tool calls fail while the TUI is
  down (by design — harnesses retry).
- **Diff pipeline.** git2 hunks → similarity line-pairing → grapheme intraline
  emphasis → syntect whole-file highlight sliced onto diff lines → composite
  (syntax-fg over diff-bg over emphasis-bg). GitHub-dark default theme;
  progressive render (a plain first frame is fine).
- **TUI.** neogit/doom keybindings, every binding configurable. Screens: Status
  (Head + Untracked/Unstaged/Staged sections + recent commits; stage/unstage/
  discard/commit/branch), Log, and Diff/review (file sidebar + single-file pane;
  `c` comment, `V` visual select, `r` reply/resolve, `v` viewed, `y`/`Y` yank
  feedback as markdown, `e` `$EDITOR` jump). OSC52 clipboard works over ssh/tmux.
- **Config.** TOML, XDG-layered (built-in defaults → `~/.config/diffler/config.toml`
  → `<repo>/.diffler/config.toml` → CLI flags; every flag has a config key).
  `diffler config --dump` prints the merged config with origins.
- **MCP (rmcp, streamable HTTP).** Tools: `review_status`, `get_diff`,
  `get_comments`, `list_reviews`, `reply_comment`, `propose_resolve`,
  `mark_viewed`, `wait_for_feedback`. Comments are tagged with their source.
  Agent triggering is the `wait_for_feedback` long-poll (MCP can't initiate agent
  turns); the human's "send" key unblocks it. `propose_resolve` only marks a
  comment Replied — only the human resolves it, in the TUI.
- **Non-goals.** Worktree/workspace management, forge/PR integration, agent
  orchestration, structural diff. LSP and task tracking are later milestones (`PLAN.md`).

## Distribution

- **Cut a release:** `just release-patch | release-minor | release-major`
  (`scripts/release.sh`). It prechecks (on main, clean tree, in sync with origin,
  tag free), bumps the version in lockstep across `Cargo.toml` (workspace + the
  `diffler-core` dep), `npm/diffler`, and `npm/diffler-mcp`, runs `just ci`, then
  commits, tags `vX.Y.Z`, and pushes. The version lives in the manifests; the tag
  mirrors them.
- **CI does the rest** (`.github/workflows/release.yml`, tag-triggered) via
  **OIDC trusted publishing — no stored tokens**: build 6 prebuilt targets →
  publish the GitHub release → crates.io (`diffler-core` + `diffler`) + npm
  (`@mattfillipe/diffler` binary wrapper + `diffler-mcp` proxy). The
  `package-managers` job renders + commits Homebrew (`Formula/`), Scoop
  (`bucket/`), AUR (`packaging/aur/`), and the Nix `flake.nix` (validated with
  `nix build` before committing).
- **AUR push is manual:** `just aur-publish` (`scripts/aur-push.sh`) with your
  local AUR SSH key.
- **Channels:** crates.io, npm ×2, GitHub releases, cargo-binstall, Homebrew tap,
  Scoop bucket, AUR (`diffler-bin`), Nix flake.
