# TUI Agent Testing Research — Deep Dive

Proven pipeline for AI agents to drive and verify TUI applications.

## The Core Pipeline

```
Agent → pexpect (PTY) → TUI App → Raw ANSI → pyte → Screen Buffer (text + colors + cursor) → Agent
```

This was **tested and confirmed working** — `top` was driven, scraped, and diffed.

## Tools by Category

| Need | Best Tool | Notes |
|------|-----------|-------|
| **PTY control** | `pexpect` (Python) | `spawn()`, `send()`, `read_nonblocking()` |
| **ANSI→screen buffer parsing** | **`pyte`** (Python) | `screen.display`, `.buffer`, `.cursor`, `.dirty` |
| **Headless terminal host** | `tmux` | `capture-pane -e -p` preserves ANSI escapes |
| **Structured JSON screen API** | `wezterm cli get-text` | Returns JSON with per-cell text+colors |
| **PNG screenshots of TUIs** | `kitty @ screenshot` | Requires Xvfb for headless |
| **Tape DSL for test scripts** | `vhs` (Charmbracelet) | Simple format, generates GIF/MP4 |
| **Framework-specific testing** | ratatui TestBackend | In-memory buffer, insta snapshots |
| **Recording/playback** | asciinema, ttyrec | Record interactions, replay + diff |

## Screen Buffer Access (via pyte)

```python
screen = pyte.Screen(80, 24)
stream = pyte.Stream(screen)
stream.feed(raw_ansi_output)

# Text content
row_text = screen.display[5]           # "     PID USER      PR..."

# Cell-level data
cell = screen.buffer[5][0]             # Cell(char=' ', fg='default', bg='default')

# Focus
cursor_pos = (screen.cursor.x, screen.cursor.y)

# Efficiency
changed_rows = screen.dirty             # {0, 1, 2, 5, 7, 8, 9}
```

## Framework-Specific Testing

### Ratatui (Rust — our choice)
```rust
let backend = TestBackend::new(80, 24);
let mut terminal = Terminal::new(backend).unwrap();
terminal.draw(|f| ui(f))?;
let buf = terminal.backend().buffer().clone();
assert_debug_snapshot!(buf);  // insta snapshots
```

### BubbleTea (Go)
```go
tm := testmodel.New(m)
tm.Send(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("a")})
assert.Equal(t, "expected", m.output)
```

### Textual (Python)
```python
async with app.run_test() as pilot:
    await pilot.press("a", "b", "c")
    assert app.query_one("#output").text == "expected"
```

## Complete Example: Driving a TUI

```python
import pexpect, pyte, time, difflib

def get_screen_buffer(command, cols=80, rows=24, keys=None, wait=1):
    child = pexpect.spawn(command, encoding='utf-8', dimensions=(rows, cols),
        env={'TERM': 'xterm-256color', 'COLUMNS': str(cols), 'LINES': str(rows)})
    time.sleep(wait)
    if keys:
        for k in keys:
            child.send(k)
            time.sleep(0.3)
    try:
        data = child.read_nonblocking(100000, timeout=0.5)
    except:
        data = child.before or ""
    child.sendcontrol('c')
    child.close()
    screen = pyte.Screen(cols, rows)
    pyte.Stream(screen).feed(data)
    return screen

# Compare two states
before = get_screen_buffer('my-tui-app')
after = get_screen_buffer('my-tui-app', keys=['\t', '\r'])
diff = list(difflib.unified_diff(before.display, after.display))
```
