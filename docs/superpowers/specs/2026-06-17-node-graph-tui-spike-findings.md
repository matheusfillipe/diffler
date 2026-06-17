# Findings: node-graph TUI spike

Date: 2026-06-17
Spec: 2026-06-17-node-graph-tui-spike-design.md

## Outcome: GO. Build the `Layered` engine — ascii-dag ranks, we render.

The spike validates the idea — a navigable orthogonal graph in the terminal is
pleasant and useful. The decisive engine is **`Layered`**: use `ascii-dag` only
for **rank/order** (Sugiyama: which column, ordered to reduce crossings) and
**draw the GitHub look ourselves** — rounded outlined boxes laid out
left-to-right, wired by clean orthogonal rails with arrowheads, junctions merged
into `├ ┬ ┼`. ascii-dag's own `[label]` art was rejected (no boxes, squiggly
fan-in); tui-nodes' own layout is worse and can't scroll. So neither engine's
*renderer* survives — only ascii-dag's *ranking*. The `graph` subcommand renders
`--demo` (CI), the real `release.yml`, `--code` (a cyclic call graph), and
live-watches a run.

## Engine bake-off

| | Layered (chosen) | ascii-dag art | tui-nodes |
|---|---|---|---|
| Layout | ascii-dag Sugiyama ranks | same ranks | crowds/overlaps; collides |
| Nodes | rounded **outlined boxes** | bare `[label]` (rejected) | bordered boxes |
| Edges | clean LR rails, merged junctions, arrows | squiggly fan-in | tangled; `α` artifact |
| Orientation | left-to-right (GitHub-like) | top-down | mixed |
| Scroll / large | we own a viewport — scrolls | scrolls | none; overflows |
| Cycles | back-edge return rail | n/a | n/a |
| Deps | ascii-dag (ranks only) | ascii-dag | dropped |

The snapshots tell it: the user rejected ascii-dag's `[label]`/squiggly output;
tui-nodes is unreadable on the same graph and can't scroll. Taking only
ascii-dag's **ranking** and drawing our own boxes + rails gives the GitHub look
with full control over boxes, status borders, selection, scroll, and cycles.

## Code graphs (the `--code` demo)

A cyclic call graph (`eval`/`apply` mutual recursion) renders: forward edges as
LR rails, the back edge as a return rail below the boxes (arrow up). It proves
non-DAG graphs work. **Caveat:** denser/cyclic graphs show some rail-through-box
overlap because routing uses one shared channel per column gap and straight
skip-level runs. CI DAGs (`--demo`, `release.yml`) render cleanly. Productionizing
the renderer needs **better edge routing** — per-edge channels and simple
obstacle avoidance — before code graphs look as clean as the CI ones, and a
later **ego-centric/radial** layout will suit "usages of X" better than columns.

## What shipped in the spike

- `graph::model` — a general **directed graph** (cycles allowed; a cyclic test
  proves non-DAG layout doesn't panic).
- `graph::engine` — `GraphEngine` trait + `Layered` (ascii-dag ranks → our own
  box/rail renderer with junction merging + back-edge return rails). The trait
  is the swap seam.
- `graph::github` — workflow `needs` → DAG, `gh run view` status overlay (matrix
  legs aggregated worst-wins); YAML parse + overlay are pure + unit-tested.
- `graph` view — vim nav (`hjkl` nearest, `n/N` follow edges, `g/G` ends),
  scrolling viewport, status-colored nodes, selection highlight.
- live watch — background `gh` poll every 5s on a blocking thread → channel →
  in-place status refresh; positions stay put.
- `tui_nodes` — comparison render (test-only) for the bake-off snapshot.
- **zoom** (`+`/`-`) — level-of-detail: compact (`[label]` overview), normal
  (boxes), detail (boxes + status-word line). Metrics drive the renderer; the
  router handles any box height.
- **collapse** (`c`) — fold a matrix group (`test`) into one `test (N)` node,
  worst-status, edges rewired; toggle to expand. Demoed on the `--demo` matrix.

## Still open (the "even more")

- GitHub matrix-leg expansion: the GitHub backend currently aggregates a matrix
  into one node; to *expand* into legs (`build (aarch64-…)`) it needs leg nodes
  + fanned edges, then collapse/expand reuses the generic group machinery.
- Better dense-graph edge routing (track assignment) for code graphs.
- Zoom-in could show real meta (durations) once the model carries it.
- Ego-centric/radial layout for LSP reference maps.

## Confirmed / learned

- ascii-dag renders nodes as `[label]`, not bordered boxes, and its layout IR
  coordinates align cell-for-cell with the rendered art — so coloring nodes by
  status is a clean overlay on the art via the IR rects.
- Status glyphs are baked into ascii-dag's art, so a live refresh currently
  re-runs `lay_out` (cheap, deterministic → positions stable). Productionizing
  could decouple the glyph from the art (reserve a cell, overlay it) to refresh
  status without re-rendering the art at all.
- Cyclic graphs lay out fine (ascii-dag breaks cycles); LSP reference/usage maps
  will still want an **ego-centric/radial** `GraphEngine` later — a new impl
  behind the same trait, no model change.

## Recommended productionization (next, separate work)

1. Promote `model` + a `CiBackend` trait (GitHub impl) into `diffler-core`
   (pure logic); keep the view + engine in the binary.
2. Keep `ascii-dag` behind `GraphEngine`; drop the tui-nodes comparison.
3. Wire a real `graph` entry into the app (not just a subcommand): a CI view,
   then DOT import (`graphviz-rust`) and a mermaid flowchart-subset front-end.
4. Decouple status glyph from layout for zero-recompute live refresh.
5. Later: LSP-fed call/reference graphs with an ego-centric layout engine.

## Cost

~1 day of spike work; ~600 LOC behind a `graph` subcommand, isolated from the
review loop, fully removable. Three new deps (`ascii-dag`, `tui-nodes`,
`serde_yaml`); only `ascii-dag` + `serde_yaml` would survive productionization
(`tui-nodes` is dropped with the comparison).
