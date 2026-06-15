# diffler

> Work in progress. Not published yet.

Terminal code review for AI coding agents. A neogit-style TUI you launch in a
repo alongside Claude Code or any MCP-compatible agent: it shows a live diff of
what the agent is doing, you stage, commit, and leave comments, and the agent
reads your feedback over the embedded MCP server and responds in place. One
binary, no browser, no daemon.

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

| Key | Action |
| --- | --- |
| `j` / `k` | move down / up (status rows, sidebar files, or diff lines) |
| `TAB` | fold (status) / switch sidebar ⇄ diff focus (diff view) |
| `<cr>` | open diff for the file, section, or commit; in the diff view, switch pane focus |
| `D` | open the full review diff |
| `<c-n>` / `<c-p>` | next / previous file in the diff view |
| `s` / `u` | stage / unstage |
| `S` / `U` | stage all / unstage all |
| `x` | discard (with confirmation) |
| `cc` | commit |
| `b` | branch popup |
| `ll` | log |
| `c` | comment on the cursor line |
| `V` | select a line range, then `c` to comment it |
| `r` / `R` | reply to / resolve a comment |
| `v` | mark file viewed |
| `y` / `Y` | copy feedback markdown (file / all) |
| `e` | open the file in `$EDITOR` |
| `Z` | send feedback to the agent |
| `q` | back / quit |

The diff view is two panes: a file sidebar and the selected file's diff. `TAB`
moves focus between them; `j`/`k` change the selected file from the sidebar or
scroll the diff when focused there; `<c-n>`/`<c-p>` switch files from either.

`?` shows the full keymap for the current screen. `<c-d>`/`<c-u>` scroll a half
page; `<c-f>`/`<c-b>` scroll a full page in the diff and log views.

The mouse works too (including over tmux): the wheel scrolls the pane under the
pointer, and a left click selects a row — clicking a section, directory, or
recent-commits header folds it, and clicking a sidebar file opens it. Mouse
capture means the terminal's own text selection needs the usual override
(`Shift`, or `Option` in iTerm2).

## MCP tools

`review_status`, `get_diff`, `get_comments`, `reply_comment`,
`propose_resolve`, `mark_viewed`, `wait_for_feedback`.

## Configuration

Layered TOML: defaults, then `~/.config/diffler/config.toml` (XDG respected,
macOS included), then `<repo>/.diffler/config.toml`, then CLI flags. Inspect
the merged result and where each value came from:

```sh
diffler config --dump
```

All keys with their defaults: [docs/config.example.toml](docs/config.example.toml).

## Development

```sh
just ci     # fmt + clippy + tests, same gate as CI
just e2e    # PTY end-to-end suite (needs uv)
```

Requires Rust 1.88+, `just`, `cargo-nextest`. Hooks: `prek install`.

## License

MIT or Apache-2.0, at your option.
