# Pane search: vim-style `/` across the status, diff, and log panes

Date: 2026-06-16

## Summary

A vim-like `/` search available in every pane. `/` opens a prompt on the bottom
bar; matches highlight live as you type; `Enter` jumps to the first match and
keeps the highlights; `n`/`N` walk forward/backward; `Esc` cancels. It is a
highlight-and-jump search (nothing is hidden), scoped to the **focused pane**,
and built on a small `Searchable` contract each pane implements so future panes
become searchable the same way.

## Goals

- One search model everywhere: file tree, diff code, file sidebar, log.
- Live incremental highlight; clear current-vs-other-match distinction.
- Keyboard-only, vim muscle memory: `/`, `n`, `N`, `Enter`, `Esc`.
- A reusable per-pane contract so a new screen is searchable by implementing one
  trait + painting highlights — no bespoke search code per pane.

## Non-goals

- No filtering / hiding of non-matches (a possible later mode for the file tree).
- No regex (plain substring first; regex is a later option).
- No cross-pane search (the diff sidebar and code pane search independently,
  whichever has focus).
- No project-wide / cross-screen search; search clears on screen switch.

## Decisions (locked in brainstorming)

- **Model:** vim highlight + jump, not a filter.
- **Scope:** the focused pane (diff sidebar when it has focus, else the code).
- **Case:** smartcase — an all-lowercase query is case-insensitive; any uppercase
  character makes it case-sensitive.
- **Matching:** plain substring.

## Behavior

- `/` (`SearchStart`) opens the prompt on the bottom bar (`/query`), capturing
  text input like the existing `Modal::Input` (printable inserts, Backspace,
  Left/Right move the query cursor).
- **Incremental:** on each keystroke, matches recompute against the focused
  pane's rows and highlight live; the cursor previews the first match at/after
  its starting row.
- **Enter** commits: cursor lands on the first match at/after its start row, the
  prompt closes, highlights persist, and the bottom bar shows `/query  [i/N]`.
- **`n`** (`SearchNext`) / **`N`** (`SearchPrev`) move to the next/previous match,
  wrapping at the ends, updating `[i/N]`. No-op when no search is active.
- **Esc** while the prompt is open cancels: clears the query + highlights and
  returns the cursor to where it started. (Committed highlights persist until a
  new search or a screen switch — `Esc` then does its normal `Back`.)
- No matches: the bottom bar shows `/query  [0/0]`; the cursor does not move.

## Architecture

### `Searchable` contract

Each focusable pane implements:

```rust
pub trait Searchable {
    /// Matchable rows as (stable row index, display text). The index is the
    /// same one the pane's renderer iterates and `focus_row` accepts.
    fn search_rows(&self) -> Vec<(usize, String)>;
    /// Move the cursor/selection to `row` and scroll it into view.
    fn focus_row(&mut self, row: usize);
}
```

- `StatusView` (tree rows: paths, section headers, recent-commit subject/author),
  `LogView` (subject/author/oid), and `DiffView` implement it. For `DiffView` the
  implementation reads `focus`: sidebar → file rows, else the diff body rows.
- This mirrors the mouse-gesture contract: one trait, exhaustive per-pane wiring,
  so a new pane is searchable by implementing it.

### Search state + engine

```rust
pub struct Search {
    query: String,
    query_cursor: usize,        // char index, for editing the prompt
    open: bool,                 // prompt capturing input vs committed
    matches: Vec<Match>,        // sorted by (row, start)
    current: usize,             // index into matches
    origin_row: usize,          // cursor row to restore on cancel
}

struct Match { row: usize, range: Range<usize> } // byte range within the row text
```

- `App.search: Option<Search>`, cleared on screen switch.
- A pure engine `find_matches(rows: &[(usize, String)], query: &str) -> Vec<Match>`
  applies smartcase + substring scan; unit-tested in isolation.
- Matches recompute when the query changes and on model refresh (so highlights
  stay valid after the diff updates), bounded by the row count.
- `n`/`N` adjust `current` (wrapping) then call `focus_row(matches[current].row)`.

### Input routing

`App::handle` gains a search layer above the keymap, like the modal layer:
- `search.open` → keystrokes edit the query (insert/backspace/left/right),
  `Enter` commits, `Esc` cancels.
- otherwise the new actions `SearchStart` / `SearchNext` / `SearchPrev` are
  resolved through the per-screen keymap (configurable; default `/`, `n`, `N`,
  bound in status, diff, and log).

### Rendering

- Two new theme colors in all three palettes: `search` (every match) and
  `search_current` (the active match — stronger). Added beside `annotated`.
- The bottom bar shows the prompt (`/query`) while open and `/query  [i/N]` while
  committed, taking over the status-message line.
- Each pane's row renderer asks the search state for `ranges_for(row)` and
  `is_current(row, range)` and paints those byte ranges with `search` /
  `search_current` as a background. Search background takes precedence over the
  diff/emphasis/annotated backgrounds on matched characters so a match is always
  visible. For the diff code lines this composites exactly like the existing
  intra-line emphasis, just a different color and higher precedence.

## Error handling / edge cases

- Query with no matches: `[0/0]`, cursor unchanged, highlights empty.
- Match becomes stale after a refresh: matches recompute against the new rows; a
  match whose row vanished drops out, and `current` clamps into range.
- Multibyte text: matching and ranges are byte-based and clamped to the row, like
  the emphasis renderer; no mid-codepoint slicing (reuse the existing
  boundary-safe compositor).
- Empty query (e.g. `/` then `Enter`): no matches, prompt closes, nothing
  highlighted (acts as a clear).

## Testing

- Pure `find_matches`: smartcase on/off, multiple matches per row, byte ranges,
  no-match, unicode.
- `Search` navigation: `n`/`N` wrap; `current` clamps after a row drops out.
- TUI snapshots: prompt open (`/query`), committed with `[i/N]`, `n` moving the
  cursor/scroll. Assert the `search`/`search_current` backgrounds appear in the
  rendered buffer (text-only snapshots don't capture color, like the emphasis
  and annotated tests).
- Per-pane `Searchable`: `search_rows` covers the expected text; `focus_row`
  moves the cursor and scrolls it into view.
- `just ci` green; `just e2e` after the TUI changes (add a PTY case: `/` + query
  + `Enter` lands on a match, `n` advances).

## Sequencing

Can land incrementally on one branch:

1. Engine + state + keymap actions + prompt input + bottom-bar prompt; wire the
   `Searchable` trait and the **status** pane (proves the model end-to-end).
2. `DiffView` (sidebar + code) and `LogView` `Searchable` impls + the in-line
   match highlighting + theme colors.
3. `n`/`N` navigation polish + `[i/N]` count + edge cases + e2e.

## Risks

- **Input routing** is the trickiest part (prompt-open vs committed-nav vs
  passthrough to the pane). Keep it a thin layer above the keymap, mirror the
  modal layer, and unit-test the state transitions.
- **`Esc` overlap** with `Back`: resolved by only consuming `Esc` while the
  prompt is open; committed highlights persist until a new search or screen
  switch.
- **Refresh churn:** recomputing matches on every model refresh must stay cheap
  (substring scan over visible rows); bound it and skip when no search is active.
