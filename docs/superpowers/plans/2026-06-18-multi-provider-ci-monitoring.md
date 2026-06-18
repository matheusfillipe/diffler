# Multi-provider CI Monitoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `diffler-ci` crate that fetches CI runs across providers behind one async trait, with GitHub + GitLab adapters (CLI-only), and wire it into the binary as Runs/Graph/Logs screens with live polling.

**Architecture:** `diffler-ci` (provider-agnostic, async `CiProvider` trait, `CommandRunner` subprocess seam, `Capabilities` flags) yields normalized `CiRun`/`RunDetail`/`LogChunk`. The binary maps `RunDetail → diffler_graph::Model`, drives polling via the existing Tick→pending→AppEvent loop, and renders three screens. No HTTP in the MVP — both adapters shell to `gh`/`glab`.

**Tech Stack:** Rust (edition 2024, MSRV 1.88), `async-trait`, `tokio`, `serde`/`serde_json`/`serde_norway`, `thiserror` (lib) / `color-eyre` (binary), `ratatui`, `insta`.

Spec: `docs/superpowers/specs/2026-06-18-multi-provider-ci-monitoring-design.md`.

---

## File structure

```
crates/diffler-ci/Cargo.toml          publish = false; deps async-trait, serde, serde_json, serde_norway, thiserror, time; dev-dep tokio(macros,rt)
crates/diffler-ci/src/lib.rs          module decls + pub use
crates/diffler-ci/src/error.rs        CiError (thiserror)
crates/diffler-ci/src/model.rs        RunId, JobId, JobStatus, CiRun, CiJob, RunDetail, LogChunk, Capabilities, DagSource, LogMode
crates/diffler-ci/src/exec.rs         CommandRunner trait, RealRunner, (test) RecordingRunner
crates/diffler-ci/src/provider.rs     CiProvider trait, ProviderKind
crates/diffler-ci/src/providers/github.rs   GitHubProvider
crates/diffler-ci/src/providers/gitlab.rs    GitLabProvider
crates/diffler-ci/src/detect.rs       detect(repo_root, remote_url, config) -> Option<(ProviderKind, Option<String>)>
crates/diffler/src/ci/mod.rs          provider factory + RunDetail→Model mapper + poll types
crates/diffler/src/ui/runs.rs         Runs start-page render
crates/diffler/src/ui/logs.rs         Logs view render
```

The existing `crates/diffler/src/graph/{mod.rs,github.rs}` are superseded: the GitHub source logic moves into `diffler-ci`, and `graph/mod.rs` keeps only `graph_theme` + the `RunDetail→Model` glue (or that moves to `ci/mod.rs`).

---

## Phase 1 — `diffler-ci` crate foundation

### Task 1.1: Crate skeleton + error type

**Files:**
- Create: `crates/diffler-ci/Cargo.toml`, `crates/diffler-ci/src/lib.rs`, `crates/diffler-ci/src/error.rs`
- Modify: root `Cargo.toml` (`members` + `[workspace.dependencies]` add `async-trait`, `time`, `diffler-ci`)

- [ ] **Step 1** — `Cargo.toml`:
```toml
[package]
name = "diffler-ci"
description = "Provider-agnostic CI run/job/log acquisition for diffler"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
publish = false

[dependencies]
async-trait.workspace = true
serde = { workspace = true }
serde_json.workspace = true
serde_norway.workspace = true
thiserror.workspace = true
time = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```
- [ ] **Step 2** — add `async-trait = "0.1"` and `time = { version = "0.3", features = ["formatting","parsing"] }` to root `[workspace.dependencies]` (verify both support 1.88: `async-trait` and `time` both do), add `crates/diffler-ci` to `members`, add `diffler-ci = { path = "crates/diffler-ci", version = "0.1.14" }`.
- [ ] **Step 3** — `error.rs`:
```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CiError {
    #[error("the `{0}` CLI is not installed or not on PATH")]
    CliMissing(&'static str),
    #[error("`{cmd}` failed: {message}")]
    Exec { cmd: String, message: String },
    #[error("parsing {what}: {message}")]
    Parse { what: String, message: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unsupported by this provider: {0}")]
    Unsupported(&'static str),
}
pub type Result<T> = std::result::Result<T, CiError>;
```
- [ ] **Step 4** — `lib.rs` module decls (`mod error; mod model; mod exec; mod provider; mod detect; mod providers;`) + `pub use` of the public types.
- [ ] **Step 5** — `cargo check -p diffler-ci`; commit `feat(ci): diffler-ci crate skeleton + error type`.

### Task 1.2: Normalized model

**Files:** Create `crates/diffler-ci/src/model.rs`

- [ ] **Step 1** — write the types (newtypes for ids, `Copy` status):
```rust
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(pub String);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus { Queued, Running, Ok, Failed, Skipped, Neutral }

impl JobStatus {
    /// Severity order so one failing leg dominates an aggregate.
    #[must_use]
    pub fn worse(self, other: Self) -> Self {
        let rank = |s: Self| match s {
            Self::Failed => 5, Self::Running => 4, Self::Queued => 3,
            Self::Skipped => 2, Self::Neutral => 1, Self::Ok => 0,
        };
        if rank(self) >= rank(other) { self } else { other }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiRun {
    pub id: RunId, pub name: String, pub branch: String, pub commit: String,
    pub author: String, pub created: Option<OffsetDateTime>, pub status: JobStatus,
    pub url: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiJob { pub id: JobId, pub name: String, pub status: JobStatus, pub needs: Vec<JobId> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDetail { pub run: CiRun, pub jobs: Vec<CiJob> }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogChunk { pub text: String, pub next_offset: u64, pub done: bool }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities { pub dag: DagSource, pub logs: LogMode }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagSource { RunApi, ConfigFile, None }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMode { Stream, Poll, Dump, None }
```
- [ ] **Step 2** — test `JobStatus::worse`: `assert_eq!(JobStatus::Ok.worse(JobStatus::Failed), JobStatus::Failed)` and `Running.worse(Ok) == Running`.
- [ ] **Step 3** — `cargo nextest run -p diffler-ci`; commit `feat(ci): normalized CI model`.

### Task 1.3: CommandRunner seam

**Files:** Create `crates/diffler-ci/src/exec.rs`

- [ ] **Step 1** — trait + real impl + recording test double:
```rust
use std::process::Command;
use crate::error::{CiError, Result};

/// Runs a subprocess and returns stdout. The seam that lets adapters be tested
/// against recorded CLI output instead of a live `gh`/`glab`.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &'static str, args: &[&str]) -> Result<String>;
}

pub struct RealRunner;
impl CommandRunner for RealRunner {
    fn run(&self, program: &'static str, args: &[&str]) -> Result<String> {
        let output = Command::new(program).args(args).output()
            .map_err(|_| CiError::CliMissing(program))?;
        if !output.status.success() {
            return Err(CiError::Exec {
                cmd: format!("{program} {}", args.join(" ")),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        String::from_utf8(output.stdout).map_err(|e| CiError::Parse {
            what: format!("{program} output"), message: e.to_string() })
    }
}
```
- [ ] **Step 2** — `#[cfg(test)]` `RecordingRunner` keyed by the first arg (e.g. `"run"`, `"ci"`) returning canned stdout from a `HashMap<&'static str, String>`; records calls for assertions.
- [ ] **Step 3** — test `RealRunner` returns `CliMissing` for a nonexistent program: `RealRunner.run("definitely-not-a-binary-xyz", &[])` → `Err(CliMissing(_))`.
- [ ] **Step 4** — `cargo nextest run -p diffler-ci`; commit `feat(ci): CommandRunner subprocess seam`.

### Task 1.4: CiProvider trait

**Files:** Create `crates/diffler-ci/src/provider.rs`

- [ ] **Step 1**:
```rust
use async_trait::async_trait;
use crate::error::Result;
use crate::model::{Capabilities, CiRun, JobId, LogChunk, RunDetail, RunId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind { GitHub, GitLab }

#[async_trait]
pub trait CiProvider: Send {
    fn kind(&self) -> ProviderKind;
    fn capabilities(&self) -> Capabilities;
    async fn list_runs(&self, limit: usize) -> Result<Vec<CiRun>>;
    async fn run_detail(&self, run: &RunId) -> Result<RunDetail>;
    async fn job_log(&self, run: &RunId, job: &JobId, offset: u64) -> Result<LogChunk>;
}
```
- [ ] **Step 2** — `cargo check -p diffler-ci`; commit `feat(ci): CiProvider trait`.

---

## Phase 2 — GitHub adapter (refactor existing logic onto the trait)

**Files:** Create `crates/diffler-ci/src/providers/{mod.rs,github.rs}`

The parse-workflow / status-mapping / matrix-aggregation logic is lifted verbatim
from `crates/diffler/src/graph/github.rs` (already unit-tested) and re-homed.

- [ ] **Task 2.1** — `GitHubProvider { runner: Box<dyn CommandRunner>, workflow_yaml: String, workflow_file: String }`. `capabilities()` → `{ dag: ConfigFile, logs: Dump }`.
- [ ] **Task 2.2** — port `parse_workflow`, `needs_of`, `map_status`, matrix `status_for` (now producing `Vec<CiJob>` with `needs`), keeping their existing tests (move them into `github.rs`).
- [ ] **Task 2.3** — `list_runs`: `runner.run("gh", &["run","list","-L",&limit,"--json","databaseId,displayTitle,headBranch,headSha,status,conclusion,workflowName,createdAt"])` → parse JSON → `Vec<CiRun>` (map status via `map_status`). Test with recorded JSON.
- [ ] **Task 2.4** — `run_detail`: `gh run view <id> --json jobs` for statuses; overlay onto the parsed-YAML jobs+needs → `RunDetail`. Test with recorded `gh run view` JSON + fixture YAML (assert needs edges + matrix aggregation, mirroring the existing two tests).
- [ ] **Task 2.5** — `job_log`: `gh run view <id> --log` (full dump); return `LogChunk { text, next_offset: text.len(), done: true }` (Dump mode ignores incremental offset). Test with recorded output.
- [ ] **Task 2.6** — `cargo nextest run -p diffler-ci`; `cargo +1.88 check -p diffler-ci`; commit `feat(ci): GitHub provider (gh CLI)`.

---

## Phase 3 — GitLab adapter

**Files:** Create `crates/diffler-ci/src/providers/gitlab.rs`

- [ ] **Task 3.1** — `GitLabProvider { runner, host: Option<String> }`. `capabilities()` → `{ dag: RunApi, logs: Poll }`. Host passed to `glab` via `--repo`/`GITLAB_HOST` (glab auto-detects from the repo otherwise).
- [ ] **Task 3.2** — `list_runs`: `glab ci list -F json -P <limit>` → parse pipelines JSON → `Vec<CiRun>` (map GitLab status strings: `success`→Ok, `failed`→Failed, `running`→Running, `canceled`→Neutral, `skipped`→Skipped, else Queued). Test with recorded JSON.
- [ ] **Task 3.3** — `run_detail`: fetch jobs + the `needs` DAG via `glab api graphql -f query='{ project(fullPath:"…"){ pipeline(iid:…){ jobs{ nodes{ id name status needs{ nodes{ name } } } } } } }'`; parse → `Vec<CiJob>` with real `needs` edges (RunApi DAG). Test with recorded GraphQL JSON.
- [ ] **Task 3.4** — `job_log`: `glab api projects/:id/jobs/:job/trace` (full trace text); return the slice from `offset` → `LogChunk { text: tail, next_offset: total_len, done: <status terminal> }`. Test with recorded trace + an offset.
- [ ] **Task 3.5** — `cargo nextest run -p diffler-ci`; `cargo +1.88 check -p diffler-ci`; commit `feat(ci): GitLab provider (glab CLI)`.

---

## Phase 4 — Detection

**Files:** Create `crates/diffler-ci/src/detect.rs`

- [ ] **Task 4.1** — `detect(repo_root: &Path, remote_host: Option<&str>, forced: Option<ProviderKind>) -> Option<(ProviderKind, Option<String>)>`. Order: `forced` wins; else host match (`github.com`→GitHub, `gitlab.com`/`*gitlab*`→GitLab, custom host with `.gitlab-ci.yml` present → GitLab with that host); else config-file presence (`.github/workflows/` → GitHub, `.gitlab-ci.yml` → GitLab). Return `(kind, host_override)`.
- [ ] **Task 4.2** — tests: github.com remote → GitHub/None; self-hosted host + `.gitlab-ci.yml` → GitLab/Some(host); no signals → None; `forced` overrides. Use a `tempfile` repo dir (dev-dep) or pass file-presence via injected closure to keep it pure.
- [ ] **Task 4.3** — commit `feat(ci): provider detection from remote + config files`.

---

## Phase 5 — Binary glue

**Files:** Create `crates/diffler/src/ci/mod.rs`; modify `crates/diffler/Cargo.toml`, `crates/diffler/src/config.rs`, `crates/diffler/src/lib.rs`.

- [ ] **Task 5.1** — add `diffler-ci.workspace = true` to the binary; `mod ci;` in `lib.rs`.
- [ ] **Task 5.2** — `ci/mod.rs`: `pub fn provider(kind, host, repo_root) -> Box<dyn diffler_ci::CiProvider + Send>` (constructs GitHub/GitLab with `RealRunner`; GitHub reads the discovered workflow YAML via the existing `discover_workflow`).
- [ ] **Task 5.3** — `ci/mod.rs`: `pub fn to_model(detail: &RunDetail) -> diffler_graph::Model` — nodes from jobs (`JobStatus`→`NodeStatus`), edges from `needs`. Unit-test the mapper (jobs+needs → nodes+edges, status mapping).
- [ ] **Task 5.4** — `config.rs`: add `pub ci: CiConfig` to `Config`; `CiConfig { provider: String /* "auto"|"github"|"gitlab" */, poll_seconds: u64, gitlab: CiProviderConfig }`, `CiProviderConfig { host: Option<String> }`, with `Default` (`provider="auto"`, `poll_seconds=5`). Add to the raw deserialize struct + `render_dump`. Test defaults + a TOML round-trip.
- [ ] **Task 5.5** — commit `feat(ci): binary provider factory, model mapper, config`.

---

## Phase 6 — Live poll + events

**Files:** modify `crates/diffler/src/event.rs`, `crates/diffler/src/app/mod.rs`, `crates/diffler/src/main.rs`.

- [ ] **Task 6.1** — `event.rs`: add `AppEvent::CiRuns(Vec<diffler_ci::CiRun>)`, `CiRunDetail(diffler_ci::RunDetail)`, `CiLog { text: String, next_offset: u64, done: bool }`.
- [ ] **Task 6.2** — `app/mod.rs`: fields `runs: Vec<CiRun>`, `open_run: Option<RunId>`, `pending_ci: Option<CiRequest>` (enum: `Runs`, `Detail(RunId)`, `Log{run,job,offset}`); `CI_POLL_TICKS` derived from `poll_seconds`; on `Tick` set the appropriate `pending_ci` for the active CI screen; handle the three `AppEvent`s into state.
- [ ] **Task 6.3** — `main.rs`: after the `pending_graph_poll` block, add a `pending_ci` block that builds the provider once (cache on `App`) and `tokio::spawn`s the matching async call, sending the result event. (Provider call is async — spawn, not spawn_blocking.)
- [ ] **Task 6.4** — commit `feat(ci): tick-driven live polling wired through the app loop`.

---

## Phase 7 — UI: Runs + Logs screens (Graph reused)

**Files:** Create `crates/diffler/src/ui/{runs.rs,logs.rs}`; modify `app/mod.rs` (Screen enum, navigation, mouse, match arms), `ui/mod.rs` (draw/help/status_bar), `keymap.rs`.

- [ ] **Task 7.1** — `Screen::Runs`, `Screen::Logs` added; all `match self.screen()` arms updated (visual_active false, cursor 0, etc., like the Graph arms).
- [ ] **Task 7.2** — `keymap.rs`: rebind `Action::OpenGraph`→ replace with `Action::OpenRuns` (`"o"`), keeping the action count/array correct; update help snapshot.
- [ ] **Task 7.3** — `ui/runs.rs`: render the runs list (status glyph · name · branch · short commit · author · relative age, right-aligned age like the commit list); selection highlight. `TestBackend` + insta snapshot from a fixture `Vec<CiRun>`.
- [ ] **Task 7.4** — `app`: Enter on a run → load `run_detail`, build the graph model via `ci::to_model`, push `Screen::Graph` (reusing the existing `GraphView`). Enter on a graph job → push `Screen::Logs` for that job.
- [ ] **Task 7.5** — `ui/logs.rs`: scrollable text view, follow-while-running indicator; `TestBackend` + insta snapshot.
- [ ] **Task 7.6** — `cargo insta test --review` for the new + churned snapshots (read diffs); commit `feat(ci): runs + logs screens; open graph from a run`.

---

## Phase 8 — Retire the old GitHub graph source

**Files:** modify/delete `crates/diffler/src/graph/{mod.rs,github.rs}`.

- [ ] **Task 8.1** — repoint the Graph screen's "open discovered workflow" path (if kept) at `diffler-ci`; delete `graph/github.rs` and the `load`/`refetch`/`GraphPoll` logic now superseded by `ci/`. Keep `graph_theme` (move to `ci/mod.rs` or `ui/graph.rs`).
- [ ] **Task 8.2** — update `event.rs` (`AppEvent::GraphModel` may be replaced by `CiRunDetail`), `main.rs` (`pending_graph_poll` removed), and any tests/snapshots.
- [ ] **Task 8.3** — `just ci` green; `cargo +1.88 check --workspace --all-features` green; `just e2e` green.
- [ ] **Task 8.4** — commit `refactor(ci): retire standalone graph github source in favour of diffler-ci`.

---

## PR strategy

Open one PR per coherent group with green CI before squash-merge:
- **PR A** — Phases 1–4 (`diffler-ci` crate, both adapters, detection; self-contained, fully unit-tested, no binary changes).
- **PR B** — Phases 5–8 (binary integration, screens, retire old source).

Each PR: `just ci` + `just e2e` + `cargo +1.88 check --workspace --all-features` green; `/rev` before opening.

## Self-review

- **Spec coverage:** trait/model/Capabilities (P1), GitHub+GitLab adapters (P2,P3), detection (P4), config (5.4), CLI-first auth (adapters shell only — no token code), live poll fits existing loop (P6), Runs/Graph/Logs (P7), CommandRunner mock + snapshots (throughout), retire old source (P8). Logs included (P2.5/3.4/7.5). All spec sections map to a phase.
- **Type consistency:** `JobStatus` (ci) vs `NodeStatus` (graph) mapped only in `ci::to_model`. `RunId`/`JobId` newtypes used in trait + events consistently. `Capabilities`/`DagSource`/`LogMode` defined once (P1.2), consumed in adapters (P2.1/3.1) and UI (P7).
- **MSRV:** `async-trait` + `time` verified ≤1.88 before adding (P1.1); `cargo +1.88 check` gates P2/P3/P8.
