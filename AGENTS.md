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
  repo.rs              repository discovery (finds the repo root from any path)
  model.rs diff.rs     diff model, hunks
  pairing.rs           similarity line-pairing + grapheme intraline emphasis
  syntax/              tree-sitter language registry + AST-diff intraline emphasis + scope index
  highlight.rs         syntect whole-file highlight
  source.rs review.rs  ReviewSource + per-source review state
  session.rs           comments + viewed marks
  store.rs             .diffler/ persistence
  feedback.rs          markdown feedback export

crates/diffler/        binary (color-eyre at the top; thiserror for typed errors):
  ui/ app/ tree.rs     ratatui TUI: screens, file sidebar, state
  app/composer.rs      in-place comment editor (app/text_edit.rs is its key set)
  ci/                  forge seam: CI acquisition + PR review (ForgeProvider trait; gh/glab/Forgejo REST)
  graph/               navigable orthogonal node-graph ratatui component
  keymap.rs config.rs  configurable keybindings, layered TOML config
  theme.rs transient.rs  rendering theme, popup/modal model
  mcp.rs               rmcp/axum MCP server
  watch.rs             notify filesystem watcher
  editor.rs clipboard.rs  $EDITOR suspend/restore, OSC52 yank
```

## Commands (just; see `just --list`)

- `just check`: clippy, run after every change
- `just test`: nextest + doctests
- `just fix`: clippy --fix + fmt
- `just snap`: insta snapshot tests; read `.snap.new` diffs before `just snap-accept`
- `just e2e`: PTY end-to-end suite (needs `uv`; CI runs it in a separate job)
- `just ci`: fmt+clippy+tests gate, must pass before any commit (CI additionally runs msrv, deny, typos, dupes, machete, coverage)
- `showcase/record.sh`: regenerate `showcase/img/*.png`, one screenshot per theme
  (needs `vhs`). It seeds a throwaway repo with a review and shoots the diff
  screen, so rerun it after anything that changes how that screen looks. The
  README's hero `assets/demo.gif` is hand-recorded and has no script.

## Rules

- Code is done only when `just ci` passes. Run it, don't assume.
- No `unwrap`/`panic!`/`todo!` in non-test code (clippy denies). `expect` needs justification.
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
- Commit messages: short, imperative, one line. No body unless the why is non-obvious.
- Comments explain why, never what. No change-history commentary.
- New dependencies: add to `[workspace.dependencies]`, justify in the commit.

## Architecture & decisions

- **Layering.** Nothing above the `Vcs` trait may import git2. Only the git2
  backend exists; the trait is there because jj is planned, but no second
  backend is built or stubbed (YAGNI).
- **Runtime.** One tokio runtime: MCP server (axum, `127.0.0.1:{port}/mcp`),
  notify watcher (debounce ~200ms → refresh), main task = the ratatui loop.
  `App` owns all state; workers (git, CI, editor, clipboard, refresh,
  enrichment) are spawned off "pending" slots and answer over the event
  channel. Watcher refreshes and per-file enrichment (emphasis/highlight/
  scope) run on the blocking pool: the pane renders plain until results
  land; draw never computes. Caches: hash-memoized per-file hashes, enriched
  models, commit/range models, CI workflow YAML. Perf guard: `just bench`
  (criterion, recorded on main by CI) + `tests/e2e/test_perf.py` ceilings.
- **Review state is per diff source.** A `ReviewSource` is `WorkingTree`,
  `Commit{oid}`, `Range{oldest,newest}`, `Pr{number}`, or `Against{rev}`.
  Comments (anchored to file + line +
  a `line_text` snapshot so stale anchors show as outdated; visual mode anchors a
  range; status Open/Replied/Resolved + threads) and GitHub-style viewed marks
  (keyed by file content hash, auto-cleared on change) are stored **per source**:
  `.diffler/reviews/<key>.json` where key ∈ {`working`, `commit-<oid>`,
  `range-<a>-<b>`, `pr-<n>`, `against-<rev>`}. Legacy `.diffler/session.json`
  migrates to `reviews/working.json`.
  `.diffler/` self-gitignores. No daemon: agent tool calls fail while the TUI is
  down (by design, harnesses retry).
- **Three-dot review (`Against{rev}`).** `d` on the status screen opens the diff
  transient: the base branch, `HEAD~1`, a branch, a commit from the log, or back
  to the plain working tree. The diff is `merge-base(rev, HEAD)` vs
  index + worktree + untracked, so the whole branch reads as one review,
  uncommitted work included, with no PR. `rev` is stored as the human named it
  and resolved at diff time, so the review follows the ref. The model is live,
  not pinned like a commit's: `App::against_rev` rides along with every queued
  refresh and `Review::compute_refresh` rebuilds it on the blocking pool, then
  `apply_refresh` swaps it into the open view (fingerprint-guarded, cursor and
  folds kept). Keys collapse `/` to `-`, so `feat/x` and `feat-x` share a
  review file.
- **Diff pipeline.** git2 hunks → similarity line-pairing → grapheme intraline
  emphasis → syntect whole-file highlight sliced onto diff lines → composite
  (syntax-fg over diff-bg over emphasis-bg). GitHub-dark default theme;
  progressive render (a plain first frame is fine).
- **Grammars.** `syntax::registry::REGISTRY` is one process-wide `LazyLock`
  holding every bundled grammar; a language compiles its highlight query on
  first use (~15ms) behind a `OnceLock`, on the enrichment thread. Registering
  a grammar is therefore free until someone opens that language, and a theme
  switch reuses the compiled queries. Some grammars extend another (`cpp` over
  `c`, `svelte` over `html`, `tsx` over `js`+`ts`): register the concatenation
  or the query silently matches almost nothing. `every_language_colours_a_sample`
  in `highlight.rs` is the guard.
- **TUI.** neogit/doom keybindings, every binding configurable. Screens: Status
  (the branch band, a rule, then the repo band; stage/unstage/
  discard/commit/branch), Log, Diff/review (file sidebar + pane, unified or
  `|`-toggled side-by-side; `c` comment, `V` visual select, `r` reply/resolve,
  `v` viewed, `y`/`Y` yank feedback as markdown, `e` `$EDITOR` jump). Comments,
  replies and edits are written in place: the composer occupies the rows the
  finished card will, under the anchored line, at the top of the file for a
  whole-file comment, under the thread for a reply. Runs (the
  CI run list), Graph (CI run detail on the shared node-graph component), Prs
  (open PRs of the repo's forge), and CiLog (a
  job's log folded into its real steps). The diff sidebar has two layouts
  (`t` cycles): tree, and review (to-review vs a folded viewed bucket,
  membership derived from the hash-keyed viewed marks so an edited file falls
  back into to-review). The status screen keeps the flat magit list. OSC52
  clipboard works over ssh/tmux.
- **Status bands.** The branch band holds the working-tree sections, Unpushed
  (commits no remote-tracking ref contains, walked to `UNPUSHED_LIMIT` and
  counted `N+` at the ceiling), the branch's own PR, and Recent commits. A bare
  rule then opens the repo band: Branches, Open pull requests (fetched the
  first time the group unfolds), CI runs. A group is present when the repo can
  have the thing at all, so zero is an answer and only a repo without remotes
  loses its Unpushed section. `[`/`]` step group headers, `tab` folds one;
  a commit carrying CI runs takes a `▸` between its glyph and sha and unfolds
  them beneath it. Every async arrival (CI poll, PR fetch, watcher refresh)
  re-seats the cursor through `status_cursor_anchor`, keyed by identity
  (path, oid, branch name) rather than row index.
- **Config.** TOML, XDG-layered (built-in defaults → `~/.config/diffler/config.toml`
  → `<repo>/.diffler/config.toml` → CLI flags; every flag has a config key).
  `diffler config --dump` prints the merged config with origins.
- **MCP (rmcp, streamable HTTP).** Tools: `review_status`, `get_diff`,
  `get_comments`, `list_reviews`, `reply_comment`, `propose_resolve`,
  `mark_viewed`, `wait_for_feedback`. Comments are tagged with their source.
  Agent triggering is the `wait_for_feedback` long-poll (MCP can't initiate agent
  turns); the human's "send" key unblocks it. `propose_resolve` only marks a
  comment Replied. Only the human resolves it, in the TUI.
- **PR review.** `ReviewSource::Pr{number}` keys review state on the PR number
  (survives pushes); the diff is `merge-base..head` via `Vcs::tree_diff`,
  fetching `refs/pull/<n>/head` when the head isn't local: reviewing never
  needs a checkout. The branch's PR is a status row; `b p` lists all open PRs
  (Enter reviews, `b` checks out). Forge review comments sync into the session
  (`remote_id` marks forge-owned rows); local comments and replies post back
  through queued workers (GitHub via `gh`, GitLab via `glab api`, Forgejo over
  its REST API). A Forgejo thread has no handle of its own, so it is the
  comments sharing a review, a path and a signed line, rooted at the lowest id;
  the forge exposes no resolution API, so `Capabilities::resolve_threads` is
  false there and a resolve stays in the local session.
- **GitLab merge requests.** A thread is a discussion and a comment is one of
  its notes, so a reply, an edit and a delete all route through the discussion
  the note belongs to, which `discussion_of` looks up. An anchored note repeats
  the merge request's `diff_refs` (base, start, head) plus the line, and a
  multi-line one adds a `line_range`. Writes travel as multipart form fields:
  GitLab's REST layer unflattens `position[new_line]` into nested parameters,
  which a JSON body never gets. A submitted review is draft notes plus one
  `bulk_publish`, so the author is notified once; the verdict maps onto
  approve/unapprove, the only review state the REST API records.
- **Non-goals.** Worktree/workspace management, agent orchestration,
  structural diff, task tracking.

## Distribution

- **Cut a release:** `just release-patch | release-minor | release-major`
  (`scripts/release.sh`). It prechecks (on main, clean tree, in sync with origin,
  tag free), bumps the version in lockstep across `Cargo.toml` (workspace + the
  `diffler-core` dep), `npm/diffler`, and `npm/diffler-mcp`, runs `just ci`, then
  commits, tags `vX.Y.Z`, and pushes. The version lives in the manifests; the tag
  mirrors them.
- **CI does the rest** (`.github/workflows/release.yml`, tag-triggered) via
  **OIDC trusted publishing (no stored tokens)**: build 6 prebuilt targets →
  publish the GitHub release → crates.io (`diffler-core` + `diffler`) + npm
  (`@mattfillipe/diffler` binary wrapper + `diffler-mcp` proxy). The
  `package-managers` job renders + commits Homebrew (`Formula/`), Scoop
  (`bucket/`), AUR (`packaging/aur/`), and the Nix `flake.nix` (validated with
  `nix build` before committing).
- **AUR push is manual:** `just aur-publish` (`scripts/aur-push.sh`) with your
  local AUR SSH key.
- **Channels:** crates.io, npm ×2, GitHub releases, cargo-binstall, Homebrew tap,
  Scoop bucket, AUR (`diffler-bin`), Nix flake.
