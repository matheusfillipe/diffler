# Diffstat counts + status-screen coloring

Two UX gaps observed in real use (screenshots):

1. No additions/deletions counts anywhere — wanted GitHub-style `+N -M` on the
   diff-pane file header (top-right), on each status section header, and a grand
   total at the top of the status screen.
2. The status screen's magit-style inline-expanded diffs aren't syntax-colored
   (they call `render_hunk_lines` with no syntax), and the file rows aren't
   colored by status — both make it hard to read vs the fully-colored diff pane.

## Part A — diffstat counts

- **Core** (`model.rs`): `impl FileDiff { pub fn diffstat(&self) -> (usize, usize) }`
  returning `(added, deleted)` counted over `hunks`'s `DiffLine` kinds. Unit test.
- **Diff pane header** (`ui/diff.rs` `pane_header_line`): right-align ` +{add}`
  (green) ` -{del}` (red) for the file, GitHub-PR style, after the existing
  path/status/comment-count. Dim a zero side.
- **Status section headers** (`ui/status.rs` `header_spans` for `SectionHeader`):
  append ` +A -D` summed over that section's files (green/red, omit a zero side
  or dim it). Recent commits header: no diffstat.
- **Status total** (`ui/status.rs`, after `head_line`): a summary line
  ` Changes  +TOTAL -TOTAL  <bar>` summing untracked+unstaged+staged. Include a
  compact proportion bar (~5 cells) colored green:red by ratio (like the
  screenshot). Skip the line entirely when the tree is clean.

## Part B — color the status screen

- **Shared status color**: `status_color(theme, FileStatus) -> Color` currently
  lives in `ui/diff.rs`. Hoist it to a shared spot both screens use (`ui/mod.rs`
  or `theme.rs`); keep the diff sidebar using it.
- **File rows** (`ui/status.rs` `file_spans`): color the status label
  (`FileStatus::label`, replacing the local `mode_text`) by `status_color`
  instead of dim. Keep the path styling as-is.
- **Inline diffs** (`ui/status.rs` `body` → `render_hunk_lines`): render the
  expanded file's hunks with syntax highlighting + diff colors, identical to the
  diff pane. Add a per-file highlight cache to the status view (mirror the diff
  view's `ensure_file_highlights` + `FileHighlights`), fill it lazily for
  expanded files, and pass the per-line syntax into the hunk renderer. Extend
  `render_hunk_lines` (or call `render_diff_line` with the syntax slice) so the
  inline diff composites syntax fg + diff bg + emphasis, exactly like the pane.
  Emphasis is already enriched lazily for expanded status files — keep that.

## Gate

`just ci` (366) + `just e2e` (14) green. Update snapshots (section headers gain
` +N -M`, pane header gains the count, a total line appears, inline diffs gain
syntax color, file rows colored) — read every `.snap.new`, confirm the diffstat
math and coloring, describe them. No `Vcs`/core API churn beyond `diffstat`.

## Out of scope

Per-file counts in the narrow sidebar (space); the gear/settings glyph in the
screenshot; bars anywhere except the status total.
