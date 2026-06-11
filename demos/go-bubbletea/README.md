# diffler demo — Go + Bubble Tea v2 + Lip Gloss v2

Mock UI demo per `../SPEC.md` (fake data, no git/MCP/LSP).

## Run

```sh
go run .
```

or build once and run the binary:

```sh
go build -o diffler-demo . && ./diffler-demo
```

Keys: `j/k` move · `Tab` cycle section/panel · `Enter` open/select · `c` comment ·
`a` accept hunk · `x` reject hunk · `q` back/quit. Mouse: click selects files,
wheel scrolls the diff.

## Versions

- Go 1.26
- `charm.land/bubbletea/v2` v2.0.7
- `charm.land/lipgloss/v2` v2.0.3

## Lines of code

- `main.go`: 954 total, ~820 excluding comments/blanks (single file)

## What was easy

- Bubble Tea v2's `tea.View` makes alt-screen + mouse + window title declarative:
  set `v.AltScreen`, `v.MouseMode = tea.MouseModeCellMotion`,
  `v.BackgroundColor` on the returned view and the runtime handles enter/restore.
  Clean restore on `q`/ctrl+c is free.
- The Elm architecture (model/update/view) keeps two screens + modal comment
  input trivially manageable in one file.
- Mouse events are typed (`tea.MouseClickMsg`, `tea.MouseWheelMsg`) with cell
  coordinates — mapping click → row → file/diff-line is simple arithmetic.
- Lip Gloss styles compose well for chips, rounded-border comment boxes, and the
  powerline-ish status bar.

## What was painful

- **Full-background theming is manual.** Lip Gloss styles only paint the cells
  they render, so every row must be explicitly padded to terminal width with
  bg-colored spaces (`fill` helper), and every style needs an explicit
  `Background(...)` or you get terminal-default holes. `v.BackgroundColor`
  helps, but per-row fill is still needed for panel/selection backgrounds.
- **Per-rune styling fights the style API.** Syntax coloring + diff-line bg +
  intra-line emphasis bg means three style dimensions per rune; had to compute
  fg/bg arrays per rune and emit grouped `lipgloss.Render` runs by hand
  (`renderCode`). No built-in span/segment model.
- **Selected-row restyle.** Re-rendering home rows on a different background
  required rewriting the SGR bg sequence in already-rendered strings (`reBg`) —
  a hack, but cheaper than rendering every row twice.
- No syntax highlighter in the framework — Python token coloring is hand-rolled
  (keywords, strings, numbers, call sites, ALL_CAPS constants). Good enough for
  a mock; a real app would use chroma.

## Select-to-copy story

Bubble Tea has no framework-native text selection. Because the app enables
mouse cell-motion tracking, the terminal hands mouse events to the app instead
of doing its own selection. Fallbacks:

- **Plain terminal:** hold **Shift while dragging** (Option+drag on some macOS
  terminals, e.g. iTerm2) to bypass app mouse tracking and use the terminal's
  native selection/copy.
- **tmux:** with `set -g mouse on`, use tmux copy-mode instead
  (`prefix + [`, select, `y`), or Shift+drag for the outer terminal's selection
  (which copies the visible pane text, including the tmux chrome).

## tmux quirks found

- Works inside tmux (`TERM=tmux-256color` or `screen-256color`); truecolor needs
  `set -ga terminal-overrides ",*256col*:Tc"` in `.tmux.conf` or the GitHub-dark
  palette gets quantized to 256 colors (visibly wrong diff backgrounds).
- Mouse wheel/click reach the app only with tmux `mouse on`; with `mouse off`
  tmux translates wheel to arrow keys on alt-screen apps, which still scrolls
  here only because j/k-style arrows are bound.
- Shift+drag selects across pane borders (it's a terminal-level selection), so
  copying diff text from a split pane grabs neighboring pane content; use tmux
  copy-mode for pane-accurate selection.

## Verification

`verify.py` drives the binary in a PTY (pexpect + pyte, 120x40,
`TERM=xterm-256color`) and asserts both screens render and `q` exits cleanly:

```sh
go build -o diffler-demo . && uv run --with pexpect --with pyte python3 verify.py
```
