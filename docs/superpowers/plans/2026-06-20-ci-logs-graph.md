# CI logs (foldable, keymap-driven) + graph extras panel — plan

Branch `feat/ci-status-section`. `/rev` before each build phase; push once at the
end with green `just ci` + `cargo +1.88 check` + `just e2e`.

## Goal 1 — foldable, keymap-driven CI Logs view

**Data.** `gh run view <run> --log --job <db>` emits step-delimited text: each
line is `<job>\t<step>\t<timestamp> <content>`. Group consecutive lines by the
step column → one collapsible section per step. Strip ANSI escapes and the
`job\tstep\ttimestamp` prefix for display. (The raw REST job-logs endpoint is
*not* step-delimited — `##[group]` ≠ steps — so use `--log`.) Step order is the
log order. (`--json jobs.steps` status/duration is a later enhancement; V1 keys
off the `--log` step column.)

**Model (`crates/diffler/src/app/logs.rs`, new):**
```
struct LogStep { name: String, lines: Vec<String>, folded: bool }
struct LogsView { steps: Vec<LogStep>, cursor, scroll, visual_anchor, viewport, body }
```
Visible rows = per step: a header row (▸/▾ name) + (if !folded) its line rows.
**Folded by default.** `LogsView::parse(raw_log)` builds the steps.

**Reuse (per the codebase map) — make Logs a first-class keymap screen:**
- keymap.rs: `Context::Logs`, `LOGS_DEFAULTS` (j/k, gg/G, `<c-d>/<c-u>/<c-f>/<c-b>`,
  `<tab>`/`za` fold, `V` visual, `/`,`n`,`N`, `y`/`Y` yank, `?`, `q`), `Keymaps.logs`.
- mod.rs: `Screen::Logs.context() = Context::Logs`; **remove `handle_logs_key`**;
  let `handle_key → dispatch → dispatch_logs` drive it.
- `dispatch_logs`: motions (reuse the `*_page` viewport math pattern), `ToggleFold`,
  `VisualSelect`, yank (selected step/lines as text via `pending_clipboard`).
- Wire the existing search infra: add Logs arms to `focused_search_rows`
  (one entry per visible row's text), `focus_searched_row`, `focused_cursor_row`,
  `visual_active`, the Esc-visual handler, and `pop_screen`.
- ui/logs.rs: render the foldable rows with cursor highlight + search highlight +
  visual selection (mirror the status/diff row rendering helpers; reuse
  `highlight_spans`). No bespoke scroll.

## Goal 2 — graph-page extras panel (artifacts + annotations)

Below the DAG, a panel showing the run's **artifacts** and **annotations**. (Job
step-summaries are NOT API-exposed — omitted by design.)

**Data (diffler-ci):** add to the model `RunExtras { artifacts: Vec<Artifact>,
annotations: Vec<Annotation> }` and a `CiProvider::run_extras(run) -> RunExtras`
(GitHub: `gh api runs/{id}/artifacts` + per-job `check_run_url` →
`check-runs/{id}/annotations`, strip ANSI in messages; GitLab: empty for now).
`Artifact { name, size, expired }`, `Annotation { level, title, message, path }`.

**Wiring:** when a run's graph opens, also request extras (a new `pending_ci`
variant + `AppEvent::CiExtras`), store on `App`, and render a bottom panel in
`ui/graph.rs` **below** the existing graph area — the DAG rendering itself is
untouched. Foldable/scrollable if it grows; keep it simple first.

## Order & gates
1. `/rev` → build Goal 1 (logs) → tests + snapshot → `just ci`/`e2e`.
2. `/rev` → build Goal 2 (graph panel) → tests + snapshot → `just ci`/`e2e`.
3. Final `/rev`, then one push; fix PR CI.

## Reuse / no-reinvention notes
- Logs motions/search/visual/yank reuse the keymap Actions + `search.rs` — no new
  scroll/motion code beyond a `logs_page` mirroring `log_page`.
- `gh api` for extras (no reqwest); ANSI-strip helper shared between logs and
  annotation messages.
