# Push / pull / fetch (Phase 2)

Add network git ops as which-key transients, shelling to the user's `git` so
their existing auth (SSH agent, credential helper, tokens) just works — no
credential code in diffler. Run async (pending-request + spawn_blocking, like the
$EDITOR suspend) so the UI never freezes during a network call.

## Backend (Vcs trait, GitVcs)

The Vcs trait owns the git-specifics; the bin runs the process generically.

- `Vcs::network_argv(&self, op: NetworkOp) -> Vec<String>` returns the argv to run
  (e.g. `["git","push"]`, `["git","push","-u","origin","HEAD"]`, `["git","pull"]`,
  `["git","fetch"]`, `["git","fetch","--all"]`). A future jj backend returns
  `["jj","git","push"]` etc. NetworkOp enum: `Push`, `PushSetUpstream`, `Pull`,
  `Fetch`, `FetchAll`.
- Also expose the repo working dir to run in (already have repo_root / workdir).
- Do NOT use git2 for these (credentials). Unit-test that network_argv returns the
  expected argv per op.

## App plumbing (mirror the editor suspend)

- `App.pending_git: Option<GitOp { label: String, argv: Vec<String> }>` set by the
  transient leaf actions.
- main.rs run loop: when `pending_git` is taken, set a status "running git push…"
  (so the next draw shows it), then `tokio::task::spawn_blocking` run
  `std::process::Command::new(argv[0]).args(&argv[1..]).current_dir(repo_root)`,
  capture stdout+stderr+status; on completion send `AppEvent::GitDone { label, ok,
  output }` into the channel. The loop keeps drawing while it runs (don't block the
  loop on the await — spawn it and continue, or await but the editor pattern shows
  await-with-restart is acceptable; prefer non-blocking: spawn the task with the tx
  clone so the result arrives as an event).
- `App::handle(AppEvent::GitDone{..})`: status bar shows success (`label` + a short
  summary) or the first non-empty stderr line on failure (Severity::Error); then
  `review.refresh()` + invalidate (head/log/ahead-behind changed). Bump nothing
  MCP-wise.

## Transients (which-key), neogit scheme

Add top-level prefixes (status context; `P`/`p`/`f` are currently free — verify no
conflict with leaves or the c/b/l prefixes):
- `P` Push: `p` Push (NetworkOp::Push), `u` Push and set upstream (PushSetUpstream).
- `p` Pull: `p` Pull (NetworkOp::Pull).
- `f` Fetch: `f` Fetch (NetworkOp::Fetch), `a` Fetch all remotes (FetchAll).

All sub-keys overridable via `[keys.push]`/`[keys.pull]`/`[keys.fetch]` like the
existing commit/branch transients; run through the same conflict enforcement. Hint
line gains `P push  p pull  f fetch` (prefix-only); `?` help lists the new groups.

## Tests

- network_argv per op (unit).
- App: a push transient leaf sets pending_git with the right argv + label; GitDone
  success shows a status + triggers refresh; GitDone failure shows the stderr line
  as an error. (Don't actually hit the network — assert pending_git/argv and the
  GitDone handler; the real run is covered loosely by an e2e against a local bare
  remote if cheap, else skipped.)
- e2e (optional, only if cheap+reliable): create a local bare remote, `git push`
  via the TUI (`P` then `p`), assert a success status — only add if it's
  deterministic; otherwise rely on the unit/argv tests + manual.
- Conflict-free defaults test still passes with the new prefixes.

## Gate

`just ci` + `just e2e` green. Snapshots: hint line gains push/pull/fetch prefixes;
which-key panels for Push/Pull/Fetch. Read .snap.new.

## Out of scope

Per-remote selection, push-to-elsewhere, force-push prompts, upstream config UI,
progress bars. The transients can grow these later.
