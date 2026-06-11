# diffler demo — TypeScript + OpenTUI

Mock UI demo per `../SPEC.md`. Fake data only; Home (magit-style status) + Diff view,
GitHub-dark full-background theme.

## Run

```sh
bun install
bun start          # or: bun src/main.ts
```

Versions: Bun 1.3.0, `@opentui/core` 0.4.0 (native Zig core, prebuilt
`@opentui/core-darwin-arm64` pulled automatically).

## Keys / mouse

- `j`/`k` (or arrows) move · `Tab` cycle panel/section focus · `Enter` open/select
- `c` opens an inline comment input under the cursor line (Enter posts, Esc cancels)
- `a` / `x` accept / reject the hunk under the cursor (verdict chip updates)
- `q` back (diff → home), `q` again quits with clean terminal restore
- Mouse: click selects rows (home, sidebar, diff lines); wheel scrolls the diff
- Text selection: mouse-drag selects (OpenTUI native), `y` copies the selection to the
  system clipboard via OSC52

## Lines of code

- App: 859 lines, single file (`src/main.ts`)
- PTY smoke test: 131 lines (`verify.py`, run with
  `uv run --with pexpect --with pyte python3 verify.py`)

## What was easy

- Flexbox layout (Yoga) — sidebar + panel + footer + status bar was painless.
- `ScrollBoxRenderable` gives wheel scrolling and a styled scrollbar for free; it also has
  `scrollChildIntoView` for keeping the keyboard cursor visible.
- `StyledText` + `fg`/`bg`/`bold` chunk combinators made per-character intra-line diff
  emphasis straightforward: split each code line at token and emphasis boundaries, style
  each segment.
- Mouse support is on by default (`useMouse: true`); per-renderable `onMouseDown` makes
  click-to-select trivial.
- `bun build --compile` just works (see below).

## What was painful

- No built-in syntax highlighter usable for plain strings at this level (there is a
  tree-sitter–based `syntax-style`/editor layer, but it's aimed at the editor renderables),
  so the Python token coloring here is a ~30-line fake regex tokenizer — per SPEC's
  "fake/minimal token coloring acceptable".
- Docs are thin; the real reference is the `.d.ts` files and the examples folder in the
  repo. API moved between 0.x versions (e.g. event wiring, input renderable options), so
  expect to read source.
- Type-checking with plain `tsc` needs `@types/node` (OpenTUI's classes extend Node's
  `EventEmitter` and the lib types reference `events`); under `bun` it runs fine without.
- Styling every chunk's `bg` explicitly is needed for full-row colored backgrounds inside
  styled text (a row-level `backgroundColor` covers the rest of the row, but chunk bg wins
  on text cells).

## Select-to-copy story

OpenTUI has **native text selection**: mouse drag highlights text across renderables
(`selectable: true` is the default on text; this demo sets `selectionBg`/`selectionFg` to
theme it). The renderer emits a `selection` event; the demo listens, stashes
`selection.getSelectedText()`, and `y` writes it to the system clipboard with an OSC52
escape — works over SSH and inside tmux (tmux ≥3.3 with `set -g set-clipboard on`).
Terminal-level fallback also works: hold Shift (or Option on macOS Terminal/iTerm) while
dragging to bypass mouse reporting and use the terminal's own selection.

## tmux quirks

- Runs fine in tmux (alternate screen, mouse SGR events, full-bg colors all OK).
- True-color needs tmux configured with
  `set -ga terminal-overrides ",xterm-256color:Tc"` (or `tmux -T RGB`); otherwise the
  GitHub-dark palette gets quantized to 256 colors and the subtle bg shades band together.
- OSC52 copy requires `set -g set-clipboard on` in tmux; OpenTUI's own drag-selection is
  unaffected since it happens app-side.
- tmux's `mode-mouse` copy mode intercepts wheel only when the app does NOT request mouse
  reporting — this app does, so wheel goes to the app as expected.

## `bun build --compile` (single binary)

Works:

```sh
bun build --compile src/main.ts --outfile diffler-demo
```

- Compile time ~0.3s, binary size **65 MB** (arm64 macOS).
- The native Zig library is embedded via OpenTUI's Bun runtime plugin; the binary was
  verified to run from an empty directory with no `node_modules` (renders both screens,
  clean exit).

## Verification

`verify.py` drives the app in a real PTY (pexpect) and asserts rendered frames via a pyte
terminal emulator: home screen contents, Enter → diff view (hunk headers, LEEWAY line,
mattf comment, verdict chips), `c` comment input open/close, `x` reject, sidebar mouse
click, SGR wheel-scroll in a 16-row PTY, and clean `q` exits. 19/19 checks pass.
