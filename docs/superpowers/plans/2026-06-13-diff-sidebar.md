# Diff view: file sidebar + single-file pane

**Goal:** Replace the diff view's continuous cross-file scroll with a two-pane
layout — a left file list (sidebar) and a right pane showing only the selected
file's diff. tuicr/lazygit model. Tab moves focus between panes; picking a file
in the list shows its diff. All M1 review features preserved.

**Why:** Multi-file diffs (commit history, full review) had no file overview —
just scroll and `<c-n>`. A sidebar gives the file set at a glance with viewed
marks and lets you jump directly.

## Current state (what changes)

`crates/diffler/src/app/diff.rs` `DiffView` flattens ALL files into one
`rows: Vec<DiffRow>` and scrolls across them; `folded: BTreeSet<String>` folds
whole files; `ui/diff.rs` renders that single list. This becomes: a file list +
a per-selected-file row list.

## Model (app/diff.rs)

```rust
pub enum Pane { List, Diff }

pub struct DiffView {
    pub source: DiffSource,
    pub(crate) commit_model: Option<DiffModel>,
    pub focus: Pane,
    pub selected: usize,          // index into model.files (the sidebar cursor)
    pub cursor: usize,            // row within the SELECTED file's rows
    pub scroll: usize,            // first visible row of the diff pane
    pub visual_anchor: Option<usize>,
    pub(crate) viewport: u16,
    pub(crate) rows: Vec<DiffRow>,   // rows for the SELECTED file only
    rows_dirty: bool,
    pub(crate) highlights: HashMap<String, FileHighlights>,
}
```

- Drop `folded` and cross-file rows. `DiffRow` keeps `Hunk/Line/Comment` but no
  longer needs `File` (the selected file is implicit); keep a header rendered by
  the pane, not as a navigable row. `Hunk`/`Line` still carry `file: usize` ==
  `selected` for reuse of existing helpers; simplest is to keep the `file` field
  set to `selected` so `diff_render`/anchor code is untouched.
- `rebuild_rows` builds rows for `model.files[selected]` only (its hunks, lines,
  and the comments anchored to that file), same comment-block logic as today.
- Selection is path-anchored across refresh: remember `selected`'s path, refind
  it after `refresh()`; clamp if gone.

## Navigation (app/diff.rs dispatch)

- **focus = List**: `j`/`k` change `selected` (rebuild rows, reset cursor/scroll
  to top of the new file); `gg`/`G` first/last file; `<cr>` or `<tab>` → focus
  Diff; review actions that need a line are no-ops here except `v` (mark viewed)
  and `e` (open file at line 1).
- **focus = Diff**: `j`/`k` move `cursor` over the file's rows (scroll follows);
  `{`/`}` hunks; `gg`/`G`/`<c-d>`/`<c-u>` within file; `<tab>` → focus List;
  `c`/`V`/`r`/`R`/`y`/`Y`/`e` as today; `v` marks viewed + advances selection.
- `<c-n>`/`<c-p>` change `selected` from EITHER pane (quick file switch), keeping
  current focus. Replaces the old section-jump meaning.
- `v` (mark viewed): mark the selected file, then advance `selected` to the next
  not-viewed file below (stay if none). Keeps the review walk.
- `q` pops the screen (unchanged).

## Rendering (ui/diff.rs)

Two-pane horizontal split: left sidebar fixed width
`min(40, max(24, area.width/4))`, right pane the rest. Sidebar rows:
`{status_glyph} {path}` truncated to width, ` ✓` dim when viewed, trailing
`· N` dim when the file has open/replied comments; selected row painted with
cursor-line bg; the focused pane's border/title gets the accent color, the
unfocused one dim. Right pane: file header line (path + status + `· comments`)
then the selected file's hunks/lines/comments via the existing `diff_render`
slice-to-visible logic. Status bar keeps `viewed M/N files`. Empty diff →
"nothing to review" in the right pane, empty sidebar.

Keep `diff_render.rs` untouched (pure helper). Reuse `comment_display`,
`anchor_target`, highlight cache, visual-selection rendering as-is, now scoped to
one file.

## Keymap (keymap.rs)

`DIFF_DEFAULTS`: `<tab>` Action::ToggleFocus (new Action), keep the rest. Remove
the file-fold meaning of TAB. `<c-n>`/`<c-p>` stay (NextSection/PrevSection
repurposed as next/prev file — or rename to NextFile/PrevFile for clarity; add
the Action variants, update help/README). gg/G/{/}/half-page unchanged.

## Scoped open (app/status.rs + app/mod.rs)

`<cr>` on a status file row → open diff with `selected` = that file, focus Diff.
`<cr>` on a section header / `D` → `selected` = first file (or section's first),
focus List. Commit-from-log → `selected` = 0, focus List.

## Tests

Unit (app/diff.rs): list j/k changes selected and resets diff cursor; Tab
toggles focus; `<c-n>` switches file from the diff pane; `v` advances selection
to next unviewed; visual select within the selected file; comment add anchors to
the selected file; refresh keeps the selected file by path when files shift;
scoped open selects the right file + focus. Snapshots (120x40): multi-file diff
with sidebar (focus Diff), focus on List, viewed ✓ + comment count in sidebar,
single-file diff (sidebar of one), commit diff from log. Read every `.snap.new`
before accepting.

## Out of scope

Resizable panes, mouse pane-drag, file tree (flat list only), per-hunk fold
within a file (dropped with cross-file fold). These are later polish.
