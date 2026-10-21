# diffler: agent guide

Terminal code-review companion for AI agents. Launched in a repo, it renders a
live neogit-style git UI and embeds an MCP server, so an agent reads review
comments in place, replies, and reacts to feedback. The human reviews and drives
git; the agent responds; the diff updates live. Philosophy: YAGNI/KISS (one
small native binary, alternate-screen TUI, no daemon, no browser).

## Layout

```
crates/diffler-core/   pure logic, no terminal (errors via thiserror):
  vcs.rs / git.rs      Vcs trait + git2 backend (status, diff, log, stage, commit, branch)
  jj.rs                JjVcs: git2 reads over jj's store, `jj` CLI for writes and jj's own view
  repo.rs              repository discovery and backend selection (git vs jj)
  model.rs diff.rs     diff model, hunks
  diffalgo.rs          selectable line-diff algorithms
  pairing.rs           similarity line-pairing + grapheme intraline emphasis
  syntax/              tree-sitter language registry + AST-diff intraline emphasis + scope index
  highlight.rs         syntect whole-file highlight
  source.rs review.rs  ReviewSource + per-source review state
  session.rs           comments (a walkthrough stop is one) + viewed marks
  walkthrough.rs       the agent's reading order: the stops' comment ids, anchors
  store.rs             .diffler/ persistence
  lens.rs              symbol lens model
  feedback.rs          markdown feedback export

crates/diffler/        binary (color-eyre at the top; thiserror for typed errors):
  ui/ app/ tree.rs     ratatui TUI: screens, file sidebar, state
  app/composer.rs      in-place comment editor (app/text_edit.rs is its key set)
  ci/                  forge seam: CI + PR review (ForgeProvider trait; gh/glab/Forgejo REST)
  graph/               node-graph component + the figures a walkthrough card draws
  keymap.rs config.rs  configurable keybindings, layered TOML config
  theme.rs transient.rs  rendering theme, popup/modal model
  mcp.rs               rmcp/axum MCP server
  workspace.rs         project tabs: one App per repository, one MCP server across them
  watch.rs             notify filesystem watcher
  editor.rs clipboard.rs  $EDITOR suspend/restore, OSC52 yank
  text.rs              display-width text shaping shared by the UI and the graph engine
```

## Commands (just; see `just --list`)

- `just check`: clippy with ci's denials, run after every change
- `just test`: nextest + doctests (needs `jj` on PATH for the jj backend's integration tests)
- `just fix`: clippy --fix + fmt
- `just snap`: insta snapshot tests; read `.snap.new` diffs before `just snap-accept`
- `just e2e`: PTY end-to-end suite (needs `uv` and `jj`; CI runs it in a separate job)
- `just package-check`: what crates.io builds. A crate packages only its own
  directory, so a file it reaches outside one builds here and fails the publish
  after the tag is public.
- `just ci`: fmt+clippy+tests gate, must pass before any commit (CI additionally runs msrv, deny, typos, dupes, machete, coverage)
- `showcase/record.sh`: regenerate `showcase/img/*.png` (needs `vhs`); rerun it
  after anything that changes how the review screen looks. The README's
  `assets/demo.gif` is hand-recorded.

## Rules

- Code is done only when `just ci` passes. Run it, don't assume.
- No `unwrap`/`panic!`/`todo!` in non-test code (clippy denies). `expect` needs justification.
- Clippy warns over cognitive complexity 20 and CI denies it. The two dispatch
  loops carry an allow and a reason; a third means the function grew a second
  job, so split it.
- Errors: `thiserror` for typed library-style errors (diffler-core, the `ci` module); `color-eyre` for the binary's top level only.
- No `println!`/stdout writes in the TUI (corrupts the screen; clippy denies it).
- Async: never block in async fns; `spawn_blocking` for CPU/IO-heavy work.
- TUI changes need TestBackend + insta snapshot coverage. A changed snapshot is a
  behavior change: read the diff, never accept blindly, never edit `.snap` by hand.
- Run `just e2e` after rendering/behavior changes: `just ci` skips it, and glyph
  or timing changes can pass ci yet break the PTY suite.
- PTY e2e probes must drain output continuously (the suite's wait helpers do);
  a bare sleep fills the PTY buffer and freezes the app under test.
- Test fixtures and sample data use generic mock names ("reviewer",
  "acme/widgets"), never real usernames, handles, or emails.
- Hooks are managed by prek (`prek install` once). If a hook fails, fix the cause.
  Never `git commit --no-verify`.
- Review before committing: in Claude Code run `/rev` on the working tree for any
  non-trivial change.
- Commit messages: one short conventional-commit line, `<type>: <lowercase
  imperative summary>` (`feat`, `fix`, `perf`, `refactor`, `test`, `docs`,
  `chore`, `ci`), e.g. `fix: speed up opening`.
- Comments explain why, never what. No change-history commentary.
- UI text is imperative and names the action: a hint is `c add comment`, never
  `c comment` (bare noun) or `c writes the first` (narrating what the program
  does). An empty state names the key that fills it. Applies to hints, messages,
  buttons and confirmations, never to code comments or docs.
- New dependencies: add to `[workspace.dependencies]`, justify in the commit.

## Architecture

- **Layering.** Nothing above the `Vcs` trait may import git2 or shell out to
  `jj`. `repo::open` picks `GitVcs` or `JjVcs` by whether the discovered root
  has a `.jj` directory.
- **Runtime.** One tokio runtime: MCP server (axum, `127.0.0.1:{port}/mcp`),
  notify watcher (debounce ~200ms → refresh), main task = the ratatui loop.
  `App` owns all state; workers (git, CI, editor, clipboard, refresh,
  enrichment) are spawned off "pending" slots and answer over the event
  channel. Refreshes and enrichment run on the blocking pool; draw never
  computes. Perf guard: `just bench` + `tests/e2e/test_perf.py` ceilings.
- **Config.** TOML, XDG-layered (built-in defaults → `~/.config/diffler/config.toml`
  → `<repo>/.diffler/config.toml` → CLI flags; every flag has a config key).
  `diffler config --dump` prints the merged config with origins.
- **Non-goals.** Agent orchestration, a difftastic-style structural layout,
  task tracking.

Before changing one of these areas, read its design notes:

- **jj** (`jj.rs`, change menu, jj push/pull, repos with no `.git`): [docs/architecture/jj.md](docs/architecture/jj.md)
- **Review sources** (`ReviewSource`, `.diffler/reviews/`, three-dot review): [docs/architecture/review-sources.md](docs/architecture/review-sources.md)
- **Diff** (pipeline, algorithms, grammars): [docs/architecture/diff.md](docs/architecture/diff.md)
- **TUI** (screens, keys, sidebars, folds, mouse, image preview, symbol lens, blame, status bands, language stats): [docs/architecture/tui.md](docs/architecture/tui.md)
- **Walkthrough** (stops, slides, anchors, figures): [docs/architecture/walkthrough.md](docs/architecture/walkthrough.md)
- **MCP** (tools, `focus`, activity indicator, instance registry): [docs/architecture/mcp.md](docs/architecture/mcp.md)
- **Forges** (PR review, PR page, GitLab, PR form, CI remote, matrix legs): [docs/architecture/forges.md](docs/architecture/forges.md)
- **Project tabs** (`workspace.rs`, event routing, worktrees): [docs/architecture/tabs.md](docs/architecture/tabs.md)
- **Releasing** (`just release-*`, CI publishing, channels): [docs/releasing.md](docs/releasing.md)
