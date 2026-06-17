# Spike: navigable orthogonal node-graph TUI (GitHub Actions first)

Date: 2026-06-17
Status: spike design — throwaway exploration to make an architecture decision.

## Why

We want a terminal UI that renders directed graphs (DAGs) as **orthogonal node
maps** — boxes wired by right-angle connectors — that a human navigates vim-style:
jump node to node, follow edges, quick-jump by name. No image protocols
(sixel/kitty), only character cells. The first concrete payload is a **live
GitHub Actions job graph**: nodes update as the run progresses, with
**status-colored borders** (green ok / red failed / yellow running / dim
queued-skipped), so you watch a pipeline execute as a graph in the terminal. A
custom multi-CI backend trait follows. The same UI substrate is later meant to
render code-as-a-graph (call/reference maps, fed by the planned LSP integration)
and to interop with DOT / a mermaid subset.

This is a **spike**: a time-boxed, behind-a-flag experiment whose job is to answer
questions, not to ship. It is acceptable for it to be rough and partly discarded.

## Questions the spike must answer

1. Is a navigable orthogonal graph in the terminal actually pleasant — can you
   read and explore a ~14-node CI DAG and a ~50-node graph without friction?
2. Which layout/render engine do we build on: **`ascii-dag`** (real Sugiyama
   layered layout + orthogonal routing, cell-snapped, draw-only) or
   **`tui-nodes`** (ratatui-native widget, fixed layout, no scroll)?
3. Is the three-layer split (model / layout / view) the right architecture, and
   does it fit diffler's trait-boundary discipline?

## Non-goals (for the spike)

- No image-in-terminal rendering of any kind.
- Not wired into the main review loop; lives behind a `graph` subcommand/flag.
- No mermaid, no DOT import yet (the model is designed to accept them later).
- No LSP / code-graph yet.
- Only one CI backend (GitHub Actions); no second backend stubbed (YAGNI).
- No persistence, no MCP surface.

## How layered (Sugiyama) layout works — the concept

A directed graph is drawn in ranks: (0) **break cycles** — a directed graph need
not be acyclic (call graphs and reference maps have recursion and mutual refs);
a DFS reverses a minimal set of "back-edges" so the rest is a DAG, and those
edges are drawn reversed afterward; (1) **assign each node a rank** by longest
path from the roots; (2) **order nodes within each rank** to minimize crossings
(median heuristic + adjacent swaps); (3) **assign x positions** to keep the
layout compact and parents centered over children; (4) **route edges** between
adjacent ranks as right-angle (Manhattan) segments — long edges pass through
dummy nodes at intermediate ranks. `ascii-dag` does all of this, handles cycles,
and emits the result snapped to integer character cells plus a `node_at(x, y)`
hit-test. This is the machinery we do not want to hand-roll.

Layered layout suits CI pipelines (genuine DAGs) well. Cyclic graphs (LSP call /
reference maps) still lay out — back-edges just curve back — but an **ego-centric
or radial** view (the focus symbol centered, callers/callees radiating) often
reads better for "usages of X." That is a different layout *strategy*, not a
different model: it slots in behind the same `GraphEngine` trait later. The spike
stays on layered layout (CI is a DAG); the model must not assume acyclicity.

## Architecture — three layers

```
Source   CiBackend trait ── GithubActions impl ─▶ Model
Layout   GraphEngine trait ─┬─ AsciiDag  (ascii-dag LayoutIR → cells)
                            └─ TuiNodes  (tui-nodes widget)
View     ratatui graph screen: viewport blit + vim nav + selection
```

Nothing above a trait knows the implementation behind it — same rule as `Vcs`.

### 1. Model (`graph::Model`)

```rust
struct Model { rankdir: RankDir, nodes: Vec<Node>, edges: Vec<Edge> }
struct Node { id: NodeId, label: String, status: NodeStatus }
struct Edge { from: NodeId, to: NodeId, label: Option<String> }
enum NodeStatus { Ok, Failed, Running, Queued, Skipped, Neutral }
enum RankDir { TopDown, LeftRight }
```

A general **directed graph** — cycles are allowed (the model must not assume a
DAG, since LSP call/reference maps are cyclic). The layout engine, not the model,
decides how to handle cycles. A deliberate subset of the fuller model the
research sketched (clusters/shapes deferred). Format-agnostic so a DOT / mermaid
/ LSP front-end can target it later.

### 2. Source — `CiBackend` trait, GitHub Actions impl

```rust
trait CiBackend { fn graph(&self) -> Result<Model>; }
```

`GithubActions` joins two sources:
- **Edges**: parse `.github/workflows/<wf>.yml` with `serde_yaml`; each
  `jobs.<id>.needs` (string or list) becomes an edge `needs → job`.
- **Status**: `gh run view <run_id> --json jobs` (subprocess; spike-acceptable)
  maps job name → conclusion/status → `NodeStatus`.
- Default target for the spike: this repo's `release.yml` + its latest run.
- Known wrinkle to handle: matrix jobs expand to multiple runtime jobs
  (`test (ubuntu-latest)` …) while the YAML has one `test` node — map by name
  prefix; document whatever we choose.

### 3. Layout/render — `GraphEngine` trait (the bake-off seam)

```rust
trait GraphEngine {
    fn render(&self, model: &Model, area: Rect, buf: &mut Buffer, sel: &Nav);
    fn nodes_at(&self, model: &Model) -> Vec<(NodeId, Rect)>; // for nav/hit-test
}
```

- `AsciiDag`: compute `LayoutIR` once (cached), draw node boxes + box-drawing
  edge segments into a virtual cell grid, blit the viewport window into `area`,
  expose laid-out node cells for navigation. Owns scrolling.
- `TuiNodes`: feed nodes/edges to the `tui-nodes` widget, render into `area`,
  read back its `split()` rects for selection. No viewport (a finding in itself).

The active engine is chosen by a flag so we can A/B the same live graph. The
comparison may be asymmetric: `ascii-dag` gets the full treatment (it is the
favoured path); `tui-nodes` gets just enough to judge its layout/edges.

### 4. View + navigation

A ratatui screen: hint line, the graph viewport, a status bar (engine name, node
count, selected node). Each node is a box whose **border color + glyph encode its
status** (green ✓ ok / red ✗ failed / yellow ⏳ running / dim queued/skipped),
with the selected node bolded/accented. Bindings reuse diffler's keymap + the `/`
search engine we just shipped:
- `h/j/k/l` — move selection to the nearest node in that direction (geometric).
- `n/N` — follow outgoing/incoming edges from the selection.
- `gg`/`G` — first/last node in topological order.
- `/` — filter/jump by label substring (reuse `search::Search`).
- `enter` — print the selected job (later: open its log).
- viewport scrolls to keep the selection visible; layout is computed off the
  render path and cached (the "render loop never computes" rule).

### 5. Live watch

While a run is in progress, poll `gh run view <run_id> --json jobs` on an
interval (~5s; CI state changes slowly) on a background tokio task, post the
refreshed statuses as an app event, and re-render. Topology is fixed during a run
— only `NodeStatus` changes — so the **layout is reused and never recomputed**;
only borders/glyphs recolor. This reuses diffler's existing event-loop +
background-task + debounced-refresh machinery. Nodes flip
queued → running → ok/failed live; a running node may show a spinner glyph.

## Success criteria

- The `release.yml` DAG (~14 nodes) renders readably with orthogonal edges and
  status-colored borders/glyphs.
- Watching a *running* `release.yml`, nodes flip queued → running → ok/failed
  live and their borders recolor, without the layout jumping (no recompute).
- `h/j/k/l` + `n/N` navigation feels fluid; the selection is always scrolled
  into view; a ~50-node graph stays navigable.
- A cyclic test graph (a small mutual-recursion call graph) lays out without
  breaking — back-edges drawn, no panic — confirming the model/engine tolerate
  non-DAGs even though CI is the primary payload.
- A short findings note records the engine decision against these axes: layout
  quality (crossings/compactness), orthogonal-edge clarity, scroll/large-graph
  behavior, cycle handling, integration effort + LOC we own, perf at 50–200
  nodes, and 0.x API stability risk.

## Risks

- `ascii-dag` is a young 0.x crate — pin the version, keep it behind the
  `GraphEngine` trait so it is swappable, add insta snapshot tests.
- `tui-nodes` has no viewport — large graphs may simply not fit; that is a
  legitimate comparison result, not a blocker.
- GitHub matrix-job name mapping and the `gh` subprocess dependency are
  spike-acceptable but must be called out, not hidden.

## Outcome → next

A go/no-go on the node UI, a chosen engine, and (if go) a productionization plan:
promote the model + `CiBackend` + `GraphEngine` + view out of the spike, drop the
losing engine, then layer on DOT import, a mermaid flowchart-subset front-end,
and eventually LSP-fed call/reference graphs.
