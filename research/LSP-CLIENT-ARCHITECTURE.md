# LSP Client Architecture for Diffler

## There Is No Magic

Every tool that "has LSP for any language" uses the same pattern:

```
1. Detect language from file extension / project root markers
2. Look up language in a static registry → get server binary name
3. Check if binary exists on PATH
4. If yes → spawn it with --stdio, speak LSP JSON-RPC
5. If no → prompt user to install
```

No tool bundles LSP servers. No tool auto-installs them silently. They all detect + prompt.

## The Registry Pattern

### Helix (`languages.toml`) — 236 server entries

The most relevant reference for a Rust TUI. Flat TOML file mapping languages to servers:

```toml
# Server definitions — binary name + args
[language-server.rust-analyzer]
command = "rust-analyzer"

[language-server.gopls]
command = "gopls"

[language-server.pyright]
command = "pyright-langserver"
args = ["--stdio"]

# Language definitions — which servers to try, in priority order
[[language]]
name = "rust"
scope = "source.rust"
file-types = ["rs"]
roots = ["Cargo.toml", "Cargo.lock"]
language-servers = ["rust-analyzer"]

[[language]]
name = "python"
scope = "source.python"
file-types = ["py", "pyi", "py3"]
roots = ["pyproject.toml", "setup.py", "pyrightconfig.json"]
language-servers = ["ty", "ruff", "jedi", "pylsp", "zuban"]  # first found wins

[[language]]
name = "go"
scope = "source.go"
file-types = ["go"]
roots = ["go.work", "go.mod"]
language-servers = ["gopls", "golangci-lint-lsp"]
```

Key insight: Python tries **5 servers in priority order**, first one found on PATH wins. This is how you get "any language" — fallback chains.

Source: https://github.com/helix-editor/helix/blob/master/languages.toml (5497 lines)

### nvim-lspconfig — 360 server configs

Lua files, one per server. Same pattern: `cmd = { 'pyright-langserver', '--stdio' }`, `filetypes = { 'python' }`, `root_markers = { 'pyproject.toml' }`.

Source: https://github.com/neovim/nvim-lspconfig

### Claude Code

Calls `which rust-analyzer` / `which pyright` etc. If found, uses it. If not, tells you to install. No LSP client library — just process spawning + JSON-RPC.

## The LSP Client You Need to Build

**No mature Rust LSP client crate exists.** `tower-lsp` is server-only. You build it yourself — it's ~500 lines.

### What an LSP client actually does

1. **Spawn**: `tokio::process::Command::new("pyright-langserver").args(["--stdio"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()`
2. **Initialize**: Send `initialize` request with `rootUri`, `capabilities`. Get server capabilities back.
3. **Listen**: Read JSON-RPC messages from stdout (`Content-Length: N\r\n\r\n{...}` framing).
4. **Route**: Match `method` field to handler, dispatch responses by `id`.
5. **Call**: Send requests (`textDocument/definition`, `textDocument/references`, `textDocument/hover`) and notifications (`textDocument/didOpen`, `textDocument/didChange`).

### Crates you DO use

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime, process spawning |
| `lsp-types` | All the LSP protocol type definitions (Request, Notification, params, results) |
| `serde` + `serde_json` | JSON-RPC serialization |
| `async-channel` or `tokio::mpsc` | Message passing between LSP client and TUI event loop |

### JSON-RPC framing (the only tricky bit)

LSP over stdio uses **Content-Length headers**, not newline-delimited JSON:

```
Content-Length: 123\r\n
\r\n
{"jsonrpc":"2.0","id":1,"method":"textDocument/definition","params":{...}}
```

You need a framed reader/writer. ~50 lines with `tokio::io::{AsyncBufReadExt, AsyncWriteExt}`:

```rust
// Read one LSP message from stdin
async fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut header = String::new();
    reader.read_line(&mut header).await?; // "Content-Length: N\r\n"
    // Skip remaining headers until blank line
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line == "\r\n" { break; }
    }
    let len: usize = header.trim().strip_prefix("Content-Length: ")
        .unwrap().trim().parse()?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

// Write one LSP message to stdin
async fn write_message(writer: &mut ChildStdin, msg: &Value) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}
```

## Runtime Installation (Prompt, Don't Install Silently)

When a language is detected but no server is found on PATH:

```
┌─────────────────────────────────────────────────────────┐
│  No LSP server found for Python.                         │
│                                                         │
│  Tried (in order):                                       │
│    ✗ ty          (not found on PATH)                    │
│    ✗ ruff        (not found on PATH)                    │
│    ✗ jedi        (not found on PATH)                    │
│    ✗ pylsp       (not found on PATH)                    │
│                                                         │
│  Recommended: install one of:                          │
│    pip install ty                                       │
│    pip install ruff                                     │
│    brew install pyright                                 │
│                                                         │
│  [i] Install ruff (pip)    [s] Skip    [q] Configure    │
└─────────────────────────────────────────────────────────┘
```

### Install commands by server (for the prompt)

```toml
[language-server.pyright]
command = "pyright-langserver"
args = ["--stdio"]
install = "npm install -g pyright"

[language-server.rust-analyzer]
command = "rust-analyzer"
install = "rustup component add rust-analyzer"

[language-server.gopls]
command = "gopls"
install = "go install golang.org/x/tools/gopls@latest"

[language-server.typescript-language-server]
command = "typescript-language-server"
args = ["--stdio"]
install = "npm install -g typescript-language-server typescript"

[language-server.jedi]
command = "jedi-language-server"
install = "pip install jedi-language-server"

[language-server.clangd]
command = "clangd"
install = "brew install llvm"  # or apt install clangd

[language-server.lua_ls]
command = "lua-language-server"
install = "brew install lua-language-server"  # or luarocks

[language-server.ruff]
command = "ruff"
install = "pip install ruff"
```

## LSP Operations Diffler Needs

| Operation | When | What it returns |
|-----------|------|-----------------|
| `initialize` | On server start | Server capabilities (which ops are supported) |
| `textDocument/didOpen` | When user opens a file in diff | Required before any other ops |
| `textDocument/hover` | User presses `K` on a line | Type info, doc string for symbol under cursor |
| `textDocument/definition` | User presses `gd` | File + line + col of definition |
| `textDocument/references` | User presses `gr` | All files + lines that reference this symbol |
| `textDocument/typeHierarchy` | User presses `gt` | Parent/child types affected by change |
| `textDocument/prepareCallHierarchy` | User presses `gc` | Call chain upstream/downstream of change |
| `workspace/symbol` | Search panel | All symbols matching query in workspace |
| `textDocument/publishDiagnostics` | Auto from server | Errors/warnings in changed files |

## Architecture Diagram

```
┌──────────────────────────────────────────────────────┐
│                    diffler TUI                         │
│                                                      │
│  ┌──────────┐  ┌───────────┐  ┌───────────────────┐  │
│  │ Diff View │  │ File List │  │ LSP Info Panel   │  │
│  └────┬─────┘  └─────┬─────┘  └────────┬──────────┘  │
│       │              │                  │             │
│  ┌────┴──────────────┴──────────────────┴──────────┐  │
│  │              Event Loop (tokio)                  │  │
│  └────┬──────────────┬──────────────────┬──────────┘  │
│       │              │                  │             │
│  ┌────┴─────┐  ┌─────┴─────┐  ┌────────┴──────────┐  │
│  │ Git Layer│  │  MCP      │  │  LSP Client Pool  │  │
│  │  (gix)   │  │  Server   │  │                    │  │
│  └──────────┘  └───────────┘  │  ┌──────────────┐  │  │
│                              │  │ rust-analyzer│  │  │
│                              │  └──────────────┘  │  │
│                              │  ┌──────────────┐  │  │
│                              │  │ pyright      │  │  │
│                              │  └──────────────┘  │  │
│                              │  ┌──────────────┐  │  │
│                              │  │ gopls        │  │  │
│                              │  └──────────────┘  │  │
│                              └────────────────────┘  │
└──────────────────────────────────────────────────────┘
```

### LSP Client Pool

- One client per language server (not per file)
- Lazy-started: spawn server on first `didOpen` for that language
- LRU or timeout-based cleanup for idle servers
- All requests go through a shared `mpsc` channel to the TUI event loop

## Syntax Highlighting (Separate from LSP)

**Use `syntect`** for highlighting (what tuicr uses). It's the right tool for this:
- 150+ languages bundled, zero config
- Pure Rust, no external deps
- Regex-based TextMate grammars (fast enough for diff views)

**Do NOT use tree-sitter for highlighting in diffler.** Tree-sitter is more powerful but requires bundling compiled `.so` grammar files + highlight queries per language. Way too much plumbing for what's essentially text coloring.

Use tree-sitter only if you later want structural diff (like difftastic). That's a separate feature.
