# Graph component library — graduating the node-graph spike

Date: 2026-06-17
Spec: supersedes the spike (2026-06-17-node-graph-tui-spike-*). Validated; now
productionize.

## Goal

Turn the validated node-graph spike into a **reusable, IO-free graph component**
in its own workspace crate (`diffler-graph`), embeddable as one view in the
binary's growing stack of views — and built so it can become a standalone
published library later. Remove all spike scaffolding (subcommand, standalone
loop, bake-off, demos-as-features).

## Decisions (locked)

- **New workspace crate `diffler-graph`** (path dependency, `publish = false`
  initially). Forces a clean, binary-free boundary; extractable/publishable as-is
  later. Depends only on `ratatui` + `ascii-dag` — no `gh`, no `serde_yaml`, no
  terminal setup.
- **Thin `View` trait now**, full window manager later (YAGNI). `GraphView`
  implements it; the binary wires it into the screen stack. The generic
  view/window manager arrives when the second custom view does.

## Architecture

```
crates/
  diffler-core/    pure logic (no terminal) — unchanged
  diffler-graph/   the component library (ratatui + ascii-dag)
    model.rs       Model / Node{ id, label, status, kind, group, foldable } / Edge / NodeStatus / NodeKind
    engine.rs      GraphEngine trait + Layered (ascii-dag ranks → our box/rail renderer)
    theme.rs       GraphTheme (just the colors the renderer needs)
    view.rs        GraphView — the reusable widget
    lib.rs         re-exports
  diffler/         binary — embeds GraphView, owns sources + the event loop
    graph_view.rs  View-trait adapter + GitHub source wiring (IO lives here)
```

### `GraphView` — the component

Owns its view state (cached `Model`, computed `Layout`, selection, scroll, zoom,
collapsed groups). It is pure: no terminal, no event loop, no IO.

```rust
impl GraphView {
    fn new() -> Self;

    // state in — the host signals state dynamically (incl. from network events)
    fn set_model(&mut self, model: Model);
    fn patch_status(&mut self, updates: impl IntoIterator<Item = (NodeId, NodeStatus)>);
    fn set_zoom(&mut self, zoom: Zoom);
    fn collapse(&mut self, group: &str, collapsed: bool);
    fn select(&mut self, id: &NodeId);

    // input — pure; returns an action for the host, never does IO
    fn on_key(&mut self, key: KeyEvent) -> Option<GraphAction>;
    fn on_mouse(&mut self, mouse: MouseEvent) -> Option<GraphAction>;

    // render — into any area, with a host-supplied palette
    fn render(&mut self, area: Rect, buf: &mut Buffer, theme: &GraphTheme);
}

enum GraphAction {
    Activated(NodeId), // Enter / double-click on a non-foldable node — host decides (open code, log…)
    Folded { group: String, collapsed: bool },
}
```

`patch_status` updates node statuses without a full rebuild when topology is
unchanged (the live-CI case); `set_model` replaces everything (topology changed).
Both keep selection by id.

### Custom nodes

`Node` carries `kind: NodeKind` (a typed enum — `Box`, `Container`, room to grow),
not `dyn Any`. The engine may render kinds differently; **per-node behavior is the
host's**, via `GraphAction::Activated(id)`. This keeps the model strictly typed and
the component free of app logic.

### Sources live in the host, not the component

GitHub/DOT/mermaid/LSP adapters do the IO and produce a `Model` (+ status deltas),
then feed `GraphView` through its state-in API. Today: a `GithubSource` in the
binary (workflow YAML + `gh` poll) calls `set_model`/`patch_status`. This is what
makes "signal state from network events" first-class and keeps the library
dependency-light. A `GraphSource` trait can formalize this in the binary now;
adding GitLab/Circle/DOT/mermaid later is one impl each.

### Theme decoupling

The component takes a small `GraphTheme { node_fg, ok, failed, running, queued,
edge, selected, … }`. The binary builds one from its `Theme`. The library never
depends on the binary's theme.

### Embedding — the `View` trait (in the binary)

```rust
trait View {
    fn render(&mut self, frame: &mut Frame, area: Rect);
    fn handle_event(&mut self, event: &AppEvent) -> Flow;
    fn title(&self) -> &str;
}
```

`GraphView` is wrapped by a binary-side adapter that implements `View`: maps the
app theme → `GraphTheme`, routes keys/mouse, and turns `GraphAction` into app
effects (e.g. open `$EDITOR` on a node). The app holds views and renders the
active one; the existing Status/Log/Diff screens are NOT migrated yet (that's the
window-manager step, deferred).

## Migration (what changes)

1. Create `crates/diffler-graph` (`publish = false`); move `model`, `engine`, and
   the rendering/nav/zoom/collapse logic of `mod.rs` into `view.rs` as `GraphView`.
2. Replace diffler `Theme` use with `GraphTheme`; add a converter in the binary.
3. Drop `run()` (standalone loop), the `graph` subcommand, and `tui_nodes.rs`
   (bake-off). Move `demo`/`code_demo` to crate examples + test fixtures.
4. Move the GitHub source (`github.rs`) into the binary as a `GithubSource` that
   feeds a `GraphView`; the live poll moves into the app's event loop.
5. Add the binary-side `View` trait + `GraphView` adapter; expose the graph as a
   real app view (entered from the app, not a subcommand).
6. Port the spike's snapshot/unit tests into `diffler-graph`; add one app-level
   integration test that drives the embedded view.

## Testing

- `diffler-graph`: pure unit tests (model collapse, ranking, label/zoom) +
  `TestBackend` snapshot tests (render, container, zoom, fold) — they move with
  the code.
- binary: an integration test that feeds a `GraphView` a model, sends events, and
  asserts the emitted `GraphAction`s + that the adapter maps theme/effects.
- `just ci` green; `just e2e` if the embedded view touches the main TUI paths.

## Non-goals (deferred)

- The generic view/window manager (migrating Status/Log/Diff). Build with the 2nd
  custom view.
- DOT / mermaid front-ends, GitHub matrix-leg expansion, durations in detail-zoom,
  ego-centric layout for reference maps, incremental relayout. All are additive
  behind the existing seams (Source / NodeKind / GraphEngine).
- Publishing `diffler-graph` to crates.io (stays `publish = false` until the API
  settles).

## Risks

- **Theme decoupling churn** — the renderer currently reads diffler `Theme`
  fields; introduce `GraphTheme` and a converter, keep snapshots stable.
- **Event routing** — the binary must route key/mouse to the active view and act
  on `GraphAction`; keep the `View` trait minimal so it doesn't fight the existing
  `App::handle`.
- **Release surface** — a new crate shares the workspace version; `publish =
  false` keeps it out of `release.yml`. Verify the release scripts skip it.
- **`ascii-dag` 0.x** — already pinned and wrapped behind `GraphEngine`; unchanged.
