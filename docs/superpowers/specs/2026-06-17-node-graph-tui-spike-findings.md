# Findings: node-graph TUI spike

Date: 2026-06-17
Spec: 2026-06-17-node-graph-tui-spike-design.md

## Outcome: GO. Build on `ascii-dag`.

The spike validates the idea — a navigable orthogonal CI graph in the terminal
is pleasant and useful — and the engine bake-off is decisive in `ascii-dag`'s
favour. The `graph` subcommand renders the demo, the real `release.yml`, and
live-watches a run.

## Engine bake-off (same model, both engines, snapshot-compared)

| | ascii-dag | tui-nodes |
|---|---|---|
| Layout | Sugiyama layered, clean, readable | crowds/overlaps; nodes collide on the demo |
| Edges | tidy orthogonal box-drawing | tangled; a port artifact (`α`) leaked |
| Coordinates | integer cells, IR aligns with the art (verified) | internal; `split()` gives node rects |
| Scroll / large graphs | we own a viewport — scrolls | none; lays out into the area, overflows |
| Status look | colored `[label]` (overlay) | real bordered boxes, per-node border style |
| Fit to our seam | `lay_out -> cells` trait, we render + navigate | self-rendering widget; different seam, less control |
| Deps / risk | zero-dep, no_std; young 0.x (pin + wrap) | ratatui 0.30 native; weak layout is the blocker |

The two rendered snapshots tell the story: `ascii-dag` produces the orthogonal
map we want; `tui-nodes` on the same 10-node graph is unreadable and clips with
no way to scroll. tui-nodes' one real advantage — bordered boxes with per-node
border colors — does not outweigh layout quality + scrolling, which are the
whole point for CI graphs and (later) larger call/reference maps.

## What shipped in the spike

- `graph::model` — a general **directed graph** (cycles allowed; a cyclic test
  proves non-DAG layout doesn't panic).
- `graph::engine` — `GraphEngine` trait + `AsciiDag` (Sugiyama layout, art grid,
  owned placements). The trait is the swap seam.
- `graph::github` — workflow `needs` → DAG, `gh run view` status overlay (matrix
  legs aggregated worst-wins); YAML parse + overlay are pure + unit-tested.
- `graph` view — vim nav (`hjkl` nearest, `n/N` follow edges, `g/G` ends),
  scrolling viewport, status-colored nodes, selection highlight.
- live watch — background `gh` poll every 5s on a blocking thread → channel →
  in-place status refresh; positions stay put.
- `tui_nodes` — comparison render (test-only) for the bake-off snapshot.

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
