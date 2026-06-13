# diffler

> Work in progress. Not published yet.

Terminal code review for AI coding agents. A neogit-style TUI you launch in a
repo alongside Claude Code or any MCP-compatible agent: it shows a live diff of
what the agent is doing, you stage, commit, and leave comments, and the agent
reads your feedback over the embedded MCP server and responds in place. One
binary, no browser, no daemon.

## Install

Not yet published; until the first release, build from source with
`cargo install --git https://github.com/matheusfillipe/diffler diffler`.
Once released:

```sh
cargo install diffler
# or
npm install -g diffler
# or download a release binary
# https://github.com/matheusfillipe/diffler/releases
```

## Quickstart

Run `diffler` inside a repository. It starts the TUI and an MCP server on
port 8417. Connect your agent once:

```sh
claude mcp add --transport http diffler http://127.0.0.1:8417/mcp
```

The loop: the agent edits files, the diff updates live. You comment lines or
ranges in the diff view and press `Z` to send feedback. The agent picks the
comments up through `wait_for_feedback`, replies or proposes resolutions, and
you confirm in the TUI. `y`/`Y` copy the same feedback as markdown if you would
rather paste it into a prompt.

## Keys

| Key | Action |
| --- | --- |
| `j` / `k` | move down / up |
| `TAB` | fold section or file |
| `<cr>` | open diff for the file, section, or commit under the cursor |
| `D` | open the full review diff |
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

`?` shows the full keymap for the current screen.

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
