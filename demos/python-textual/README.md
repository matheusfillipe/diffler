# diffler demo — Python + Textual

Mock-data implementation of `../SPEC.md`. No git/MCP/LSP; everything is embedded constants.

## Run

```sh
uv run main.py
```

(uv resolves the venv from `pyproject.toml` on first run; afterwards `.venv/bin/python main.py` also works.)

## Versions

- Python 3.14.5
- Textual 8.2.7 (pulls rich 15.0.0, pygments 2.20.0)
- uv 0.11.7

## Lines of code

- `main.py`: 673 total, 549 non-blank/non-comment. Single file, includes the
  embedded CSS theme (~80 lines of it) and all mock data.

## Keys

- `j/k` (or arrows) move cursor / file selection, mouse wheel scrolls
- `Tab` cycles panel focus (home: jumps sections; diff: sidebar ⇄ diff)
- `Enter` opens the selected changed file (home) / focuses diff (sidebar)
- `c` inline comment input on cursor line (Enter submits, Esc cancels)
- `a` / `x` accept / reject the hunk under the cursor (verdict chip updates)
- `q` back / quit; click selects rows and sidebar files

## What was easy

- Alternate screen, raw mode, clean restore, resize: free with `App.run()`.
- Full-bg GitHub-dark theming: Textual CSS (`Screen { background: #0d1117 }`,
  per-class rules for diff line backgrounds, focus-dependent rounded borders)
  did exactly what the spec asks with no manual cell painting.
- Mouse: wheel scroll and click handling are built in; `on_click` per row widget
  was a 2-liner.
- Syntax highlighting: `rich.Syntax("", "python", theme="github-dark").highlight(line)`
  gives per-line pygments tokens as a rich `Text`; intra-line emphasis is then just
  `text.stylize(Style(bgcolor=...), start, end)` over `difflib.SequenceMatcher` opcodes.
- Inline widgets in the diff: mounting an `Input` (or a comment box `Static`)
  *after* an arbitrary line widget (`panel.mount(box, after=row)`) makes inline
  comments trivial — no manual layout math.
- Headless testing: `app.run_test()` + Pilot let me drive every key path without
  a terminal before doing the PTY check.

## What was painful

- Key routing subtleties: `Screen` already binds `Tab` to focus-next, and an
  focused `Input` swallows printable keys, so the `j/k/c/a/x` bindings have to
  live on the screen (not the app) and rely on Textual's focus-first dispatch
  order. It works, but you have to know the binding-priority rules.
- Right-aligned status bar segment needed a custom `render()` doing width math;
  there's no one-line "left + right" primitive for a 1-row bar.
- The cursor line concept (one `Static` per diff line, re-rendering two widgets
  per move) is something you build yourself; Textual's list widgets
  (`OptionList`/`ListView`) don't allow arbitrary inline children like comment
  boxes, so a `VerticalScroll` of `Static`s is the workable pattern.
- Powerline glyphs (``) render only with a patched font — cosmetic only.

## Select-to-copy story

Textual has **framework-native text selection** (since Textual 3.x; verified in
8.2.7): click-drag selects text across `Static` widgets (`ALLOW_SELECT = True`
by default), and `ctrl+c` / `cmd+c` triggers `screen.copy_text`, which copies
via `App.copy_to_clipboard` (OSC 52, with a pyperclip-style fallback). So
selection works *inside* the TUI, mouse mode on, no terminal tricks — including
inside tmux if tmux has `set -g set-clipboard on` so OSC 52 passes through.
Terminal-level fallback (Shift/Option+drag depending on emulator) also still
works if you prefer raw terminal selection.

## tmux quirks

- Runs fine inside tmux (tested `TERM=xterm-256color` under a PTY; tmux sets
  `TERM=screen-256color`/`tmux-256color`, both fine — Textual degrades colors
  gracefully and 24-bit color needs `set -ga terminal-overrides ",*:Tc"` on
  older tmux or colors get quantized to 256).
- OSC 52 clipboard (the native copy above) requires `set -g set-clipboard on`
  in tmux, otherwise copy silently does nothing.
- Mouse events pass through only with tmux `mouse on` (or use tmux's own copy
  mode, which then shadows the app's mouse support while active).
- Powerline glyph rendering depends on the client terminal font, not tmux.

## Cold start & distribution

- Cold start (PTY-measured, time to first painted frame): **~0.41s** via
  `uv run main.py` (warm uv cache), **~0.33s** running the venv python
  directly. A truly cold `uv run` (first ever, resolving + installing textual)
  takes a few seconds once, then is cached.
- Distribution is the honest weak spot: a **Python runtime is required**.
  Options, frankly assessed:
  - `uv run` / `uvx` — best story today: ship the folder (or publish to PyPI),
    users need only `uv`; it provisions Python 3.14 + deps automatically.
  - **PyInstaller** — produces a single binary (~15–25 MB for a Textual app),
    works, but is per-platform, slower to start (onefile self-extracts), and
    occasionally needs hidden-import fixes after dependency bumps.
  - **pex / shiv / zipapp** — single `.pex`/`.pyz` file, but still requires a
    matching Python interpreter on the target machine, so it doesn't remove
    the runtime dependency.
  - Nothing here matches the "scp one static binary" story of Go/Rust; if that
    is a hard requirement, Python loses this category.
