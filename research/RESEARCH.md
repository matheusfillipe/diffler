# Diffler — Research Project

> TUI code reviewing tool with agentic batteries, vim motions, Magit-like UX, MCP tools, LSP integration, worktree switching.

## Architecture Decisions (Research-backed)

### Tech Stack

**Rust + ratatui + crossterm** — proven by tuicr (37k LOC) and gitui.

| Component | Technology | Rationale |
|-----------|-----------|-----------|
| TUI Framework | ratatui + crossterm | 13k★, immediate-mode, battle-tested by tuicr & gitui |
| Diff Engine | tree-sitter (structural diff) | Syntax-aware diffing across 40+ langs (difftastic approach) |
| Layout | Flexbox-style engine | Inspired by lazygit's boxlayout for Magit-like split panels |
| Git Operations | gix (pure-Rust git) | Async, no git CLI dependency |
| LSP Client | tower-lsp or custom JSON-RPC over stdio | Rust LSP client ecosystem |
| MCP Server | rmcp (Rust MCP SDK) | Expose diffler as MCP server for agent control |
| Terminal Testing | pexpect + pyte + tape DSL | Proven pattern for agent-driven TUI testing |

---

## Component Research

### 1. Existing TUI Code Review Tools

#### tuicr (https://github.com/agavra/tuicr) — **Primary Base**
- **Stack**: Rust + ratatui 0.30 + crossterm 0.29
- **37,300 lines** of well-structured Rust
- **Architecture**: `App` central state → `VcsBackend` trait (git/jj/hg) → `ForgeBackend` trait (GitHub via `gh` CLI) → `ReviewStore` public library API
- **Agent hooks**: `skills/tuicr/` agent skill bundle, `tuicr review list|add|comments` CLI (JSON output)
- **UI**: File list sidebar + unified/side-by-side diff + comment navigator
- **Missing**: No LSP, no MCP, no worktree switching

#### difftastic — **Best Diff Algorithm**
- **Stack**: Rust + raw crossterm (custom rendering)
- tree-sitter structural diff — gold standard for syntax-aware diffing
- Side-by-side AST-aligned display, word-level highlighting

#### delta — **Best Rendering/Styling**
- **Stack**: Rust + syntect (pipe-based pager)
- Word-level Levenshtein diff, side-by-side view, 20+ configurable elements

#### lazygit — **Best Layout Engine + UX**
- **Stack**: Go + custom gocui fork + tcell/v3
- Flexbox-inspired recursive layout system
- Worktree navigation, transient menus
- 1788 Go files — industry standard

#### gitui — **Proves the Stack**
- **Stack**: Rust + ratatui + crossterm (same as tuicr)
- Async git, line-level staging, fuzzy search, file system watcher

### 2. TUI Framework Ecosystem

| Rank | Framework | Language | Stars | Approach |
|------|-----------|----------|-------|----------|
| 1 | **ratatui + crossterm** | Rust | 13k | Immediate mode + widgets |
| 2 | bubbletea + lipgloss | Go | 28k | Elm architecture |
| 3 | textual | Python | 25k | Async + CSS-like |
| 4 | ink | Node | 27k | React-like |
| 5 | tview + tcell | Go | 8k | Widget-based |
| 6 | cursive | Rust | 5k | Retained mode |

**Decision**: ratatui. Same stack as tuicr means we can fork/contribute rather than build from scratch.

### 3. AI Agent TUI Testing

#### The Core Pipeline (Proven)
```
Agent → pexpect (PTY) → TUI App → Raw ANSI → pyte → Screen Buffer → Agent
```

#### Key Tools

| Need | Tool | How |
|------|------|-----|
| **PTY control** | `pexpect` (Python) | `spawn()`, `send()`, `read_nonblocking()` |
| **Screen buffer parsing** | **`pyte`** (Python) | `screen.display`, `.buffer`, `.cursor`, `.dirty` |
| **Headless terminal host** | `tmux` | `capture-pane -e -p` preserves ANSI |
| **Structured JSON screen** | `wezterm cli get-text` | Returns JSON per-cell text+colors |
| **PNG screenshots** | `kitty @ screenshot` | Vision agents can literally "see" TUI |
| **Tape DSL** | `vhs` (Charmbracelet) | Simple format, generates GIF/MP4 |
| **Framework-specific** | ratatui `TestBackend` | In-memory buffer, `insta` snapshots |
| **Recording/playback** | asciinema, ttyrec | Record interactions, replay + diff |

#### Framework-Specific Testing
- **Ratatui**: `TestBackend` writes to in-memory buffer, snapshot with `insta`
- **BubbleTea**: `teatest` package — model-based assertions, no scraping
- **Textual**: `pilot` — CSS selectors, `.screenshot()`, DOM queries
- **Ink**: `ink-testing-library` — `.find()`, `.findByText()`

#### Tape DSL (proposed for diffler)
```yaml
app: "diffler"
terminal: { cols: 120, rows: 40 }
steps:
  - wait: 0.5s
  - snapshot: "initial"
  - keys: ["j", "j", "Enter"]  # Navigate to file, open
  - wait: 0.3s
  - snapshot: "file-opened"
  - assert:
      contains: "src/main.rs"
      cursor_row: 5
  - keys: ["gd"]  # Go to definition (LSP)
  - wait: 1s
  - snapshot: "definition-jumped"
```

#### Screen Buffer Capabilities (via pyte)
- `screen.display` — visible text per row
- `screen.buffer[y][x]` — Cell with `.fg`, `.bg`, `.bold`, `.reverse`
- `screen.cursor` — focus indicator
- `screen.dirty` — changed rows (efficient diffing)

### 4. LSP Integration in TUI

#### Rust LSP Client Options
- **tower-lsp**: Full LSP server framework (server-side). Can be used for client too.
- **lsp-types**: Shared LSP type definitions for Rust
- **Custom JSON-RPC over stdio**: Most flexible — speak LSP protocol directly

#### Key LSP Features for Code Review
| Feature | Use Case in Review |
|---------|-------------------|
| `textDocument/definition` | Jump from changed line to definition |
| `textDocument/references` | Find all callers of changed function |
| `textDocument/hover` | See type/signature of changed symbol |
| `textDocument/typeHierarchy` | Understand impact of changes |
| `textDocument/prepareCallHierarchy` | Trace call chain affected by diff |
| `workspace/symbol` | Navigate to symbols in changed files |

#### Architecture
```
diffler TUI → LSP Client (stdio/pipe) → Language Server (rust-analyzer, pyright, etc.)
                    ↓
           position → definition/references
                    ↓
           Open $EDITOR at location
```

#### Mapping Diff to LSP
- Parse diff hunks to get file + line ranges
- For each changed line, request LSP hover/definition/references
- Display LSP info inline in diff view (hover popover, references panel)

### 5. MCP (Model Context Protocol) Integration

#### What MCP Gives Diffler
1. **Diffler AS MCP Server**: Agents can call `diffler_list_reviews`, `diffler_add_comment`, `diffler_navigate`, etc.
2. **Diffler AS MCP Client**: Connect to external MCP servers (GitHub, git, testing) from within the TUI

#### Rust MCP SDK
- **`rmcp`** (https://github.com/modelcontextprotocol/rust-sdk): Official Rust MCP SDK
- Provides both server and client implementations
- Transport: stdio (for CLI embedding) and HTTP+SSE

#### Proposed MCP Tools (Diffler as Server)
```
diffler_list_reviews() → [{id, title, author, files, status}]
diffler_get_diff(review_id, file) → {unified_diff, hunks}
diffler_add_comment(review_id, file, line, body) → {comment_id}
diffler_navigate(file, line) → {status} (TUI navigates)
diffler_get_context(file, line) → {hover_info, definition, references}
diffler_switch_worktree(name) → {status}
diffler_open_editor(file, line) → {status} (opens $EDITOR)
```

### 6. Git Worktree Management

#### Git Worktree CLI
```bash
git worktree add ../project-fix branch-name    # Create worktree
git worktree list                                # List all
git worktree remove ../project-fix               # Remove
git worktree prune                               # Clean stale refs
```

#### How lazygit Handles It
- Dedicated worktree panel
- Switch between worktrees by selecting
- Shows branch + path for each worktree
- Creates new worktrees from current branch

#### For Diffler
- Worktree panel showing: branch, path, status
- Switch context: all LSP/file operations target the active worktree
- Quick-create: `git worktree add <path> <branch>` from within review
- Agent can switch worktrees via MCP tool

### 7. Editor Integration ($EDITOR)

#### Opening Files at Specific Positions
```bash
# Neovim
nvim +42 path/to/file.rs           # Line 42
nvim +'call cursor(42,15)' file.rs # Line 42, col 15

# Vim
vim +42 path/to/file.rs

# VS Code
code --goto path/to/file.rs:42:15

# Helix
hx +42:15 path/to/file.rs
```

#### Pattern from Existing Tools
- lazygit: Press `e` to open file in $EDITOR at cursor line
- lazygit: Customizable via config (`gui.editor`)
- gitui: Similar `e` keybinding

#### For Diffler
- `gd` → open $EDITOR at definition location (LSP-driven)
- `gf` → open $EDITOR at file under cursor
- `ge` → open $EDITOR at current diff hunk
- Configurable `$DIFFLER_EDITOR` with fallback to `$EDITOR`

---

## Magit-Like UX Patterns to Port

### Magit's Key Interactions
1. **Status buffer** → transient states (commit, rebase, etc.)
2. **Section folding** (TAB to expand/collapse sections)
3. **Transient commands** (prefix keys → contextual popup menu)
4. **Staging hunks/lines** with granular control
5. **Diff navigation** (j/k, SPC to scroll, TAB to jump between files)

### Terminal Equivalents
| Magit Pattern | Diffler Implementation |
|---------------|------------------------|
| Section folding | ratatui Collapse widget + TAB key |
| Transient commands | Popover panel on prefix key (like lazygit) |
| Hunk staging | `t` toggle hunk, `s` stage selection |
| File tree | Sidebar with expand/collapse, vim j/k |
| Diff view | Split pane (side-by-side or unified) |
| Commit/PR list | Top panel, filter/search |

---

## Self-Testing Strategy

### Three Layers

1. **Unit/Model Tests** (Rust, no terminal): Test the App state machine, diff parsing, LSP message handling
   - `cargo test` — ratatui `TestBackend` for render verification
   
2. **Integration Tests** (tape DSL): Record TUI interactions, verify screen states
   - Custom test harness: spawn diffler in PTY, send keystrokes, capture screen, assert
   - VHS-like tapes for documentation + regression
   
3. **Agent-Driven Tests** (MCP): Agent calls MCP tools, verifies TUI responds correctly
   - Agent calls `diffler_navigate()`, then screenshots screen buffer
   - Agent adds comment via MCP, then verifies it appears in TUI

### The VTerm/Buffer Question
**Yes, there's a clean vterm buffer approach**: `pexpect + pyte` gives you a full virtual terminal buffer. The agent:
1. Spawns diffler in a PTY
2. Sends keystrokes via `pexpect.send()`
3. Reads raw ANSI output
4. Feeds it through `pyte` to get structured screen buffer
5. Diffs screen states, asserts on content/cursor/colors

This works **headless** in CI — no X11/Wayland needed.

---

## Open Questions / Further Research

- [ ] Evaluate forking tuicr vs building from scratch
- [ ] Benchmark tree-sitter structural diff on large codebases
- [ ] Test LSP client latency from within a TUI event loop
- [ ] Evaluate rmcp (Rust MCP SDK) maturity for production use
- [ ] Design the tape DSL spec (formal grammar)
- [ ] Evaluate async Rust framework for LSP (tokio vs smol)
- [ ] How to handle multiple simultaneous LSP servers (multi-language repos)
