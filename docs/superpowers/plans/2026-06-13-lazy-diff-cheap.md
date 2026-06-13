# Cheaper diff loading (no API change)

**Goal:** ~halve diff-view/status open time without the full lazy-per-file
refactor. Measured: `Review::open` = ~147ms/80 files, built twice (review model
+ status), each ~33ms build + ~24ms pairing.

Two changes, both internal:

## 1. Defer intra-line pairing to per-file TUI render

`.emphasis` (intra-line change ranges) is read ONLY by `ui/diff_render.rs`.
MCP (`render_unified`, `comment_info`) and `feedback` use origin+text, never
emphasis. So pairing is a pure TUI concern and need not run in the backend.

- `pairing.rs`: extract `pub fn enrich_file(file: &mut FileDiff)` (the per-file
  body of `enrich`); keep `enrich(model)` delegating to it.
- `git.rs::diff_to_model`: REMOVE the `crate::pairing::enrich(&mut model)` call.
  Models now come back without emphasis.
- TUI enriches lazily, memoized, before rendering a file:
  - Diff view: before rendering the selected file, enrich it once. Track
    enriched paths in a `HashSet<String>` on `DiffView`, cleared on
    `invalidate`/refresh. Enrich `model.files[selected]` (review model or the
    view's `commit_model`) via `&mut`.
  - Status view: when a file is expanded inline, enrich it once (same memo
    approach on the status view state).
- Tests: `git_backend.rs` currently asserts emphasis is present in the model
  (`modified_line_pair_carries_intraline_emphasis`, ~line 150) — change it to
  call `pairing::enrich_file` first, or assert on enrich_file directly. The TUI
  emphasis tests (diff_render hand-built hunks; the app-level fixture render)
  must still pass because the view enriches before render — verify the
  app/diff render path enriches.

## 2. Defer the working-tree review model until the diff view opens

`Review::open` eagerly computes BOTH `status` (3 areas) and the working-tree
`model`. The status screen is the initial view; the `model` (working_tree_diff)
is only needed once the diff view opens (D / section / file / commit uses its
own commit_model). Defer it:

- `Review`: make the working-tree `model` lazily computed + cached. Lowest-churn
  approach: keep a `model` cache filled on first access via a
  `pub fn model(&mut self) -> &DiffModel` accessor (computes `working_tree_diff`
  once, caches; `refresh()` invalidates the cache; `open()` no longer computes
  it). If `&mut` is awkward at a call site, use `OnceCell`/`RefCell` interior
  mutability so the accessor can take `&self`. Pick whichever keeps call sites
  clean.
- Update the few readers: `ui/diff.rs` (draw_body / `diff.model`), `app/status.rs:383`
  (path membership check), `app/mcp.rs:18` (`get_diff` whole-model render — this
  triggers lazy compute on an explicit agent request, fine).
- `status` stays eager (initial screen needs the sections). Out of scope: making
  status sections themselves lazy — that's the deferred full refactor.
- Verify behaviorally: opening to status does not compute the working model;
  opening the diff view computes it once; `refresh` while a diff view is open
  recomputes; MCP `get_diff` works.

## Gate

`just ci` (358 tests) + `just e2e` (14) green. No `Vcs` trait change. Add a test
that the working model is lazy (e.g. `Review::open` leaves the cache empty until
`model()` is called, and `model()` matches eager `working_tree_diff`), and that
MCP/feedback are unaffected by the missing eager emphasis.

## Out of scope (deferred full refactor)

Per-file lazy hunk/text computation, a `DiffSource`/`file_diff` Vcs API, lazy
status sections. Revisit if large changesets still bite after this.
