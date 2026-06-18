# Multi-provider CI monitoring — design

Status: approved design (sub-project #1 of a larger CI-monitoring effort).
Date: 2026-06-18.

## Goal

Monitor CI runs for the repo diffler is launched in, across providers, rendering
each run as a live node-graph (via the existing `diffler-graph` component) plus a
live log view. Providers are pluggable behind one trait; the first cut ships
GitHub Actions (refactored off the existing source) and GitLab CI, CLI-only.

## Why this is worth building (prior art)

Research (cloned `gh-dash`, `lazyactions`, `glim`; surveyed magit/forge and the
SaaS landscape) found **no open-source, local-first, direct-to-forge TUI that
renders the CI dependency DAG**, and none that unifies providers — the only
unified monitor (CI/CD Watch) is closed SaaS aggregating server-side. The live
DAG-in-a-terminal and the unified-local angle are both unoccupied. Lessons taken:

- **`glim`** (Rust/ratatui/tokio — our stack): an `api → service` split with an
  event bus and delta polling. We adapt its event-driven fetch model to diffler's
  single-loop pattern (no separate poller tasks).
- **`lazyactions`**: define the trait at the **CI domain** (`list_runs` /
  `run_detail` / `job_log`), and the interface you mock for tests *is* the
  provider seam. Copy its `SecureToken` redaction idea when API mode lands.
- **magit/forge**: key adapters by **API host** (not provider name) so
  self-hosted instances and Gitea≈Forgejo reuse fall out; ship **support tiers**
  (full / partial / read-only) incrementally.

### Capability reality (drives normalization)

The dependency DAG and log story differ per provider — the normalization layer is
the real work:

| Provider | DAG source | Live logs |
|---|---|---|
| GitHub | config YAML (`needs:`) | poll status / dump on completion |
| GitLab | run-API (`glab api graphql` `needs`) or YAML | stream (`glab ci trace`) |
| CircleCI | run-API (`dependencies`) | none official (gap) |
| Forgejo | run-API (`needs` on jobs) | poll |
| Woodpecker | config YAML (`depends_on`) | stream (SSE) |
| Jenkins | plugin-gated (Blue Ocean) | poll (`progressiveText`) |

## Non-goals

Pipeline execution, config translation/migration, triggering or cancelling runs
(read-only monitoring only), cross-repo dashboards, and forge PR/issue features.
These are out of scope for the whole effort, not just the MVP.

## Architecture

### Crate layout

A new crate **`diffler-ci`** (sibling to `diffler-graph`) owns provider-agnostic
CI data acquisition. It does **not** depend on `diffler-graph`: it yields
normalized CI types; the binary maps `RunDetail → diffler_graph::Model`.
Acquisition / rendering / composition stay separate.

```
crates/diffler-ci/
  model.rs           CiRun, CiJob, JobStatus, RunDetail, LogChunk, Capabilities
  provider.rs        trait CiProvider (the seam) + ProviderKind
  detect.rs          remote-host + config-file → (ProviderKind, host)
  exec.rs            trait CommandRunner (subprocess seam — the test seam)
  providers/github.rs   adapter (generalizes today's graph/github.rs)
  providers/gitlab.rs    adapter
  error.rs           thiserror CiError
crates/diffler/ (binary)
  ci/mod.rs          map RunDetail → diffler_graph::Model; poll wiring; provider construction
  ui/runs.rs         start-page (runs list);  ui/logs.rs  log view
```

### The seam — normalized model + trait

```rust
// model.rs — provider-agnostic, no IO
pub struct CiRun { pub id: RunId, pub name: String, pub branch: String,
                   pub commit: String, pub author: String, pub created: Option<OffsetDateTime>,
                   pub status: JobStatus, pub url: Option<String> }
pub struct CiJob { pub id: JobId, pub name: String, pub status: JobStatus, pub needs: Vec<JobId> }
pub enum JobStatus { Queued, Running, Ok, Failed, Skipped, Neutral } // maps 1:1 to graph NodeStatus
pub struct RunDetail { pub run: CiRun, pub jobs: Vec<CiJob> }          // jobs + needs == the DAG
pub struct LogChunk { pub text: String, pub next_offset: u64, pub done: bool }

// what a provider can do, so the UI degrades gracefully instead of lying
pub struct Capabilities { pub dag: DagSource, pub logs: LogMode }
pub enum DagSource { RunApi, ConfigFile, None }
pub enum LogMode { Stream, Poll, Dump, None }

// provider.rs — async (async-trait), Send for the app loop
#[async_trait]
pub trait CiProvider: Send {
    fn kind(&self) -> ProviderKind;
    fn capabilities(&self) -> Capabilities;
    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>, CiError>;
    async fn run_detail(&self, run: &RunId) -> Result<RunDetail, CiError>;
    async fn job_log(&self, run: &RunId, job: &JobId, offset: u64) -> Result<LogChunk, CiError>;
}
```

`CiJob.needs` is the normalization point (GitHub fills it from workflow YAML,
GitLab from `glab api graphql`). `LogChunk { offset, done }` unifies stream / dump
/ poll behind one method. `Capabilities` lets the UI hide a graph when
`DagSource::None` or a follow toggle when `LogMode::Dump`.

**Decided:** the trait is **async** (`async-trait`) — the known end-state (API
mode / Jenkins need `reqwest`) and the natural model for concurrent run + log
polling. CLI adapters implement the async methods by running the subprocess on a
blocking task via the `CommandRunner`.

### Extensibility (explicit requirement)

The crate is built to grow. Adding a provider is a closed, local change:

1. Implement `CiProvider` in `providers/<name>.rs`, declaring its `Capabilities`.
2. Add its detection signals to `detect.rs` (remote host + config files).
3. Register it in the provider factory (`ci/mod.rs` in the binary).

Nothing in `diffler-graph`, the Runs/Logs screens, or the mapper changes — they
consume the normalized model and the `Capabilities` flags. The two IO seams keep
adapters fully testable and let API mode slot in without touching the trait:

- **`CommandRunner`** — abstracts subprocess execution; CLI adapters depend on it.
- **`HttpClient`** (added with the API sub-project) — same shape for REST/GraphQL.

Support tiers are explicit: a provider may be full (runs + DAG + logs), partial
(runs + status, no DAG or no logs), or read-only — surfaced via `Capabilities`,
not hidden behind runtime failures.

### Detection + config

Detect order (cheap → costly): **git remote host** → known SaaS
(github.com / gitlab.com / codeberg.org); else **config-file candidates**
(`.github/workflows`, `.gitlab-ci.yml`, …); else (later sub-project) an
unauthenticated API probe (`/api/forgejo/v1/version`, `/api/v1/version`,
`/api/v4/version` returns 401 on GitLab). Explicit config override always wins.

```toml
[ci]
provider = "auto"     # auto | github | gitlab
poll_seconds = 5
[ci.gitlab]
host = "gitlab.example.com"   # override remote detection (self-hosted)
```

MVP config is deliberately minimal. `mode = "cli" | "api"` and `token_env` arrive
with the API sub-project — not stubbed now (the repo's no-stub rule).

### Auth

CLI-first. `gh` / `glab` auto-detect the repo's host and hold the token in their
own keyring/config; diffler reads and stores nothing, preserving "credentials
entirely out of diffler." When API mode lands, config holds the token's
**env-var name**, never the value. A missing CLI surfaces an actionable message
(the existing `run_gh` "is the GitHub CLI installed?" pattern, generalized).

### Live updates (fits diffler's existing loop)

No background poller. Reuse the established pattern: a `Tick` every `poll_seconds`
sets `pending_ci_poll`; `main.rs` spawns the provider call; the result returns as
`AppEvent::CiRuns(..)` / `CiRunDetail(..)` / `CiLog(..)`; the app swaps state.
This mirrors today's `pending_graph_poll → AppEvent::GraphModel` exactly,
generalized to runs / detail / log. Log polling advances `LogChunk.next_offset`
and stops when `done`.

### UI flow

- **`Screen::Runs`** (start page) — runs list for the repo's provider: status
  glyph · pipeline/workflow · branch · commit · author · age, reusing the
  right-aligned commit-list styling. `o` from Status opens it (reassigned from
  "graph of latest workflow"; the graph is now reached *through* a selected run).
- Select a run → **`Screen::Graph`** with that run's DAG + live status. The graph
  component already exists; it now receives a real `RunDetail`-derived `Model`.
- Select a job → **`Screen::Logs`** — scrollable, auto-follow while running,
  polling `job_log` by offset.

### Errors

`thiserror` `CiError` in `diffler-ci` (lib, like `diffler-core`): `CliMissing`,
`Exec`, `Parse`, `NotFound`, `Unsupported`. The binary maps to `color-eyre` and a
status-bar message. Per-provider failures degrade (no runs / no DAG / no logs)
rather than crash — matching today's best-effort `gh` handling.

## Testing — mock-API, full coverage

- **`CommandRunner` is the test seam.** Adapters take a runner; tests inject
  recorded `gh` / `glab` stdout — no real subprocess or network. Every adapter is
  unit-tested against fixture JSON / YAML (extends today's `github.rs` tests).
- Pure pieces — YAML→DAG, JSON→`CiRun`, `RunDetail`→`Model` mapper — unit-tested
  directly.
- A `MockProvider` (implements `CiProvider` from fixtures) drives the
  service + UI paths end-to-end.
- Runs and Logs screens get `TestBackend` + insta snapshots (repo rule); read the
  `.snap.new` diff before accepting.
- Coverage target: every adapter parse path, the mapper, detection, and each new
  screen.

## MVP cut

In: trait + model + `Capabilities` + detect + `CommandRunner` + GitHub adapter
(refactor existing) + GitLab adapter + Runs/Graph/Logs screens + live poll +
config keys above. Deferred to later sub-projects: API/`reqwest` mode, network
probe detection, CircleCI / Forgejo / Woodpecker / Jenkins adapters, cross-repo
dashboard.

## Roadmap (subsequent sub-projects, each its own spec → plan → PRs)

1. **This spec** — seam + GitHub + GitLab + Runs/Graph/Logs (CLI-only).
2. API mode: `HttpClient` seam + `reqwest` + `[ci.<p>] mode/token_env` +
   `SecureToken`; first API provider (Jenkins, no DAG-capable CLI).
3. Remaining providers incrementally (CircleCI, Forgejo, Woodpecker), each
   declaring its `Capabilities`.
4. Network-probe detection for self-hosted hosts.

## Conventions this work must follow

These are the project's standing rules; the implementation plan and every PR
adhere to them (recorded here so the spec is self-contained):

- **Done = `just ci` passes.** Run it, don't assume. `just ci` is fmt + clippy
  (`-D warnings`) + nextest + doctests. CI additionally runs **msrv (1.88), deny,
  typos** — `just ci` does *not*. A new dependency can pass `just ci` and fail the
  CI `msrv` job (e.g. its own MSRV exceeds 1.88): before pushing a dep, run
  `cargo +1.88 check --workspace --all-features` and check the crate's
  `rust_version`. `deny` (unmaintained/advisory) is also CI-only.
- **TUI changes:** run `just e2e` (the PTY suite, CI-only) in addition to
  `just ci`; rendering changes can pass ci and break e2e.
- **Review before committing:** run `/rev` on the working tree for any non-trivial
  change.
- **No `unwrap` / `panic!` / `todo!` / indexing that can panic** in non-test code
  (clippy denies). `expect` needs justification. No `println!` / stdout in the TUI.
- **Errors:** `thiserror` in `diffler-ci` / `diffler-core`; `color-eyre` only in
  the binary.
- **No stubs / no "not implemented" / no placeholder data** — `mode = "api"` is
  not added until it works.
- **Comments explain why, never what; no change-history commentary.**
- **New dependencies** go in `[workspace.dependencies]` and are justified in the
  commit; verify MSRV 1.88 (the lesson that bit the ascii-dag drop).
- **Snapshots:** a changed `.snap` is a behaviour change — read the diff, never
  accept blindly, never edit by hand.
- **Git:** feature branches only; squash-merge PRs; CI green before merge; never
  `--no-verify`. Commit messages: short, imperative, one line.
- **Extensible by construction:** adapters depend on `CommandRunner` /
  `HttpClient` seams and declare `Capabilities`; adding a provider touches only
  its adapter + detection + the factory.
