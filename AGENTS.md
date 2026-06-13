# diffler — agent guide

Rust workspace, tokio + ratatui TUI. Crates: `diffler-core` (logic, no terminal),
`diffler` (binary: TUI + MCP server). Design spec: `docs/superpowers/specs/`.
Research and decisions: `research/`. Roadmap: `PLAN.md`.

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
