# diffler demo — Rust + ratatui

Mock UI demo per `../SPEC.md` (fake data, no git/MCP/LSP). Two screens: Home
(magit-style status) and Diff view, GitHub-dark full-background theming.

## Run

```sh
cargo run
```

Keys: `j/k` move · `Tab` cycle panel focus · `Enter` open/select · `c` comment
(Enter submits, Esc cancels) · `a` accept hunk · `x` reject hunk · `q` back/quit.
Mouse: click selects (click again on the selected Home row to open it), wheel scrolls.

## Versions

- ratatui 0.30.1
- crossterm 0.29.0
- similar 2.7.0 (intra-line char diff)
- Rust edition 2021

## Lines of code

- App: 914 lines, single file (`src/main.rs`)
- Plus `verify.py` (71 lines): a pexpect+pyte PTY smoke test
  (`uv run --with pexpect --with pyte python3 verify.py`)

## What was easy

- Theming: `Style::new().fg().bg()` with RGB constants maps 1:1 to the spec
  palette; painting a full-bg `Block` over `f.area()` fills the whole terminal.
- Layout: `Layout::vertical/horizontal` with constraints plus `Block::inner()`
  made the sidebar/diff/status-bar split trivial. Rounded borders are built in
  (`BorderType::Rounded`).
- Intra-line diff: the `similar` crate's `TextDiff::from_chars` gives char-level
  ops directly; mapping them onto per-char background colors was ~15 lines.
- Lifecycle: `ratatui::init()` / `ratatui::restore()` (0.30 API) handle alternate
  screen, raw mode, and panic-safe restore; only mouse capture needs manual
  enable/disable.
- Immediate-mode rendering means scroll/cursor/ensure-visible logic is plain
  index math on a `Vec<Line>` — no virtual DOM or reflow surprises.

## What was painful

- No built-in syntax highlighter: hand-rolled a minimal Python tokenizer
  (keywords / strings / calls / UPPER_CASE consts / numbers / operators). It is
  fake-but-plausible coloring, exactly as the spec allows; `syntect` would be the
  real answer.
- Styled `Line`s don't auto-pad: to get full-width row backgrounds (selection
  bars, diff line bgs, comment boxes) every row must be manually padded with a
  styled spacer span, and width accounting is per-char (`pad_to`/`pad_between`
  helpers).
- Per-char styling for the intra-line emphasis + syntax colors means merging two
  style dimensions (fg from tokenizer, bg from diff marks) into runs of spans —
  fiddly off-by-one territory.
- Mouse hit-testing is fully manual: you must record widget rects and row→item
  maps during draw and consult them in the event handler. No widget-level click
  events exist.
- Comment boxes (rounded border + author chip + wrapping + inline cursor) are
  drawn character-by-character as text; there is no overlay/popup primitive that
  flows inline with scrolled content.

## Select-to-copy story

ratatui/crossterm has no framework-native text selection. Because the demo
enables mouse capture (click-to-select, wheel scroll), the terminal's normal
drag-select is swallowed. Fallback:

- **Most terminals** (iTerm2, Terminal.app, kitty, alacritty, wezterm): hold
  **Shift** (Option on some macOS terminals) while dragging to bypass mouse
  capture and use native selection/copy.
- **tmux**: needs `set -g mouse on`, then tmux's own copy-mode selection
  (drag, or `prefix+[`) works; Shift+drag selects through to the underlying
  terminal instead (joining panes' text, so prefer tmux copy-mode).

## tmux quirks found

- Wheel scroll and clicks only reach the app with `set -g mouse on`; otherwise
  tmux eats the wheel for its own scrollback.
- `TERM` inside tmux is `screen-256color`/`tmux-256color`; true-color RGB needs
  `set -ga terminal-overrides ",*256col*:Tc"` or the GitHub-dark palette gets
  quantized to the nearest 256 colors (visibly murkier diff backgrounds).
- The powerline glyph (`\u{e0b0}`) in the status bar needs a Nerd Font in the
  client terminal; tmux passes it through but shows tofu without one.
- Alternate-screen restore works cleanly inside tmux; no residue after `q`.
