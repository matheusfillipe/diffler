# diffler

[![crates.io](https://img.shields.io/crates/v/diffler.svg)](https://crates.io/crates/diffler)
[![npm](https://img.shields.io/npm/v/@mattfillipe/diffler.svg)](https://www.npmjs.com/package/@mattfillipe/diffler)
[![IRC](https://img.shields.io/badge/IRC-chat.h4ks.com-blue.svg)](https://chat.h4ks.com)

A tool for taking ownership of agentic code: review what your agent writes,
together with the agent, while it happens. The code that lands is code you
have actually reviewed.

diffler is a neogit-style TUI you launch in a repo alongside Claude Code or
any MCP-compatible agent. It shows a live diff of what the agent is doing;
you read, comment, stage, and commit; the agent picks your feedback up over
the embedded MCP server and responds in place. One binary, no browser, no
daemon.

## Install

The installed command is always `diffler`. Pick whatever fits:

```sh
# Rust — compile from source, or grab the prebuilt with cargo-binstall
cargo install diffler
cargo binstall diffler

# Homebrew (macOS / Linux)
brew tap matheusfillipe/diffler https://github.com/matheusfillipe/diffler
brew install diffler

# Scoop (Windows)
scoop bucket add diffler https://github.com/matheusfillipe/diffler
scoop install diffler

# Arch (AUR)
yay -S diffler-bin

# Nix (flake — runs the prebuilt binary)
nix run github:matheusfillipe/diffler
nix profile install github:matheusfillipe/diffler

# npm — prebuilt binary, one-off or global
npx @mattfillipe/diffler
npm install -g @mattfillipe/diffler
```

Or download a prebuilt binary (macOS, Linux, Windows; x86_64 and arm64) from the
[releases page](https://github.com/matheusfillipe/diffler/releases) — any
GitHub-release installer (`eget`, `ubi`, …) works against it too.

## Quickstart

Run `diffler` inside a repository. It starts the TUI and an MCP server on
port 8417. Connect your agent once:

```sh
claude mcp add --transport http diffler http://127.0.0.1:8417/mcp
# or, over stdio, auto-discovering the port:
claude mcp add diffler -- npx -y diffler-mcp
```

The loop: the agent edits files, the diff updates live. You comment lines or
ranges in the diff view and press `Z` to send feedback. The agent picks the
comments up through `wait_for_feedback`, replies or proposes resolutions, and
you confirm in the TUI. `y`/`Y` copy the same feedback as markdown if you would
rather paste it into a prompt.

## Keys

Vim-like, neogit-flavored: `j`/`k`/`gg`/`G` motions, `/` search, and
`<c-d>`/`<c-u>` paging work in every list. The basics:

| Key | Action |
| --- | --- |
| `<cr>` | open the thing under the cursor |
| `s` / `u` | stage / unstage |
| `cc` | commit |
| `c` | comment the diff line (`V` selects a range first) |
| `Z` | send feedback to the agent |
| `y` / `Y` | copy feedback as markdown (file / all) |
| `x` | in the diff view: graph who calls the symbol under the cursor |
| `e` | open the file in `$EDITOR` |
| `?` | full keymap for the current screen |
| `q` | back / quit |

Every binding is remappable — see
[docs/config.example.toml](docs/config.example.toml).

The diff view is two panes: a file sidebar and the selected file's diff. `TAB`
moves focus between them; `j`/`k` change the selected file from the sidebar or
scroll the diff when focused there; `<c-n>`/`<c-p>` switch files from either.

The mouse works too (including over tmux): the wheel scrolls the pane under the
pointer, and a left click selects a row — clicking a section, directory, or
recent-commits header folds it, and clicking a sidebar file opens it. Mouse
capture means the terminal's own text selection needs the usual override
(`Shift`, or `Option` in iTerm2).

## MCP tools

While the TUI is running, diffler serves an MCP server the agent uses to read
your review and respond in place. See [docs/mcp.md](docs/mcp.md).

## Configuration

Layered TOML: defaults, then `~/.config/diffler/config.toml` (XDG respected,
macOS included), then `<repo>/.diffler/config.toml`, then CLI flags. Every
option and key remap, documented with its default, lives in
[docs/config.example.toml](docs/config.example.toml). Inspect the merged
result and where each value came from:

```sh
diffler config --dump
```

## Development

```sh
just ci     # fmt + clippy + tests, same gate as CI
just e2e    # PTY end-to-end suite (needs uv)
```

Requires Rust 1.88+, `just`, `cargo-nextest`. Hooks: `prek install`.

## License

MIT or Apache-2.0, at your option.
