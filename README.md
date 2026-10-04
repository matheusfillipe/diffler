# diffler

**A standalone magit for the terminal: a keyboard-driven git UI and code reviewer, on its own or with your coding agent.**

[![crates.io](https://img.shields.io/crates/v/diffler.svg)](https://crates.io/crates/diffler)
[![npm](https://img.shields.io/npm/v/@mattfillipe/diffler.svg)](https://www.npmjs.com/package/@mattfillipe/diffler)
[![IRC](https://img.shields.io/badge/IRC-chat.h4ks.com-blue.svg)](https://chat.h4ks.com)

![diffler reviewing an agent's change: word-level diff highlights, an inline comment, and the agent replying and fixing the code live over MCP](assets/demo.gif)

If you like magit or vim and want that workflow outside Emacs, this is it: one
binary, [Doom Emacs](https://github.com/doomemacs/doomemacs) keys, a fast
keyboard-driven git UI. Stage and commit your own work, review local changes or
pull requests, and, when you work with a coding agent, review what it writes
while it writes it.

## What it does

- magit-style git: stage, unstage, discard, commit, branch, push and pull, with Doom Emacs keys.
- Code review in the diff: comment on a line or range, mark files viewed, compare against any branch or commit.
- Pull requests on GitHub, GitLab and Forgejo: review without checking out, sync existing threads, submit your comments as one review.
- Optional agent review over MCP: the diff updates as your agent edits, it answers your comments in the thread, and it can walk you through its change one stop at a time.
- Works in git and colocated jj repos. Shows changed images as pictures (kitty, sixel, iTerm2, or half blocks).

## Install

```sh
cargo install diffler
# or
brew tap matheusfillipe/diffler https://github.com/matheusfillipe/diffler && brew install diffler
# or
npm install -g @mattfillipe/diffler
```

<details>
<summary>Other install methods (prebuilt binary, Scoop, AUR, Nix)</summary>

```sh
# prebuilt binary, no Rust toolchain needed
cargo binstall diffler

# Scoop (Windows)
scoop bucket add diffler https://github.com/matheusfillipe/diffler
scoop install diffler

# Arch
yay -S diffler-bin

# Nix
nix run github:matheusfillipe/diffler
nix profile install github:matheusfillipe/diffler
```

Binaries for macOS, Linux and Windows (x86_64 and arm64) are on the
[releases page](https://github.com/matheusfillipe/diffler/releases).

</details>

## Quick start

```sh
diffler                 # open the status screen in the current repo
diffler path/to/repo    # or in another one
```

Inside diffler, `<cr>` opens a file's diff, `s` stages, `cc` commits, and `c`
comments a line. For pull requests, `b` `p` lists the open ones, `<cr>`
reviews one, and `S` submits your comments as a single review. `b` `P` opens a
new pull request from the current branch.

### With a coding agent

diffler serves MCP on port 8417 while it runs. Connect Claude Code once:

```sh
claude mcp add --transport http diffler http://127.0.0.1:8417/mcp
```

Comment on the agent's changes and press `Z` to send them. The agent answers
in the thread and you resolve it once you're happy. Ask it for a walkthrough
of its change and open it from the status screen. More in
[docs/walkthroughs.md](docs/walkthroughs.md). Don't need it? Set
`enabled = false` under `[mcp]` in the config, or run `diffler --no-mcp`.

<details>
<summary>Other ways to connect an agent (Claude Code plugin, opencode, any MCP client)</summary>

**Claude Code plugin**, which adds `/df` to answer your comments, `/dfa` to walk
you through a change, and `/dfr` to review a change and leave comments:

```sh
claude plugin marketplace add matheusfillipe/diffler && claude plugin install diffler@diffler
```

**Over stdio**, finding the running diffler's port on its own:

```sh
claude mcp add diffler -- npx -y diffler-mcp
```

**opencode**: add the server to `opencode.json`, then fetch the commands:

```json
{
  "mcp": {
    "diffler": {
      "type": "local",
      "command": ["npx", "-y", "diffler-mcp"],
      "enabled": true
    }
  }
}
```

```sh
mkdir -p ~/.config/opencode/commands
for c in df dfa dfr; do curl -fsSLo ~/.config/opencode/commands/$c.md \
  https://raw.githubusercontent.com/matheusfillipe/diffler/main/.opencode/commands/$c.md; done
```

**Any other MCP client**: point it at `http://127.0.0.1:8417/mcp` (streamable
HTTP), or run `npx -y diffler-mcp` as a stdio proxy. Prompt-aware clients also
get `review`, `walkthrough` and `critique` commands. Tools are listed in
[docs/mcp.md](docs/mcp.md).

</details>

## How it compares

| | diffler | [tuicr](https://github.com/agavra/tuicr) | [lazygit](https://github.com/jesseduffield/lazygit) | [magit](https://magit.vc) |
|---|:---:|:---:|:---:|:---:|
| Standalone binary | ✅ | ✅ | ✅ | ❌ needs Emacs |
| Stage, commit, branch, push | ✅ | ❌ | ✅ | ✅ |
| Comment on diff lines | ✅ | ✅ | ❌ | ❌ |
| Agent answers in the thread, live | ✅ MCP | markdown export | ❌ | ❌ |
| Agent walkthroughs of a change | ✅ | ❌ | ❌ | ❌ |
| Inline PR review | GitHub, GitLab, Forgejo | GitHub, GitLab, Gitea, Bitbucket, Azure DevOps, Gerrit | ❌ | ❌ |
| jj | ✅ colocated | ✅ | ❌ | ❌ |
| Vim / Doom Emacs keys | ✅ | ✅ vim | partial | ✅ |

## Keys

Vim motions everywhere (`j`/`k`, `gg`/`G`, `/`, `<c-d>`/`<c-u>`). `?` shows the
full keymap of the screen you're on, and `<c-k>` fuzzy-finds any action.

| Key | Action |
| --- | --- |
| `<cr>` | open the thing under the cursor |
| `s` / `u` | stage / unstage |
| `cc` | commit |
| `c` | comment the line (`V` selects a range first) |
| `Z` | send comments to the agent |
| `m` | mark the file viewed |
| `t` | switch the sidebar layout |
| `\|` | toggle side-by-side |
| `]` / `[` | next / previous hunk |
| `za` | fold or open the hunk |
| `d` | on the status screen, diff against another branch or commit |
| `C` | open the comments list |
| `S` | submit PR comments as one review |
| `gf` | find any file |
| `B` | blame |
| `e` | open in `$EDITOR` |
| `q` | back / quit |

Every binding is remappable in [docs/config.example.toml](docs/config.example.toml).
The mouse works too, over tmux included.

## Configuration

Layered TOML: defaults, then `~/.config/diffler/config.toml`, then
`<repo>/.diffler/config.toml`, then CLI flags. Every option is documented in
[docs/config.example.toml](docs/config.example.toml). See the merged result and
where each value came from:

```sh
diffler config --dump
```

## Themes

Switch live with `T`, or set `ui.theme`. See every theme in the
[gallery](showcase/THEMES.md).

![the review screen in catppuccin-mocha: the file tree, a diff carrying a comment thread with the agent's reply, and the comments sidebar](showcase/img/catppuccin-mocha.png)

## Development

```sh
just ci     # fmt + clippy + tests
just e2e    # PTY end-to-end suite (needs uv)
```

Requires Rust 1.90+, `just`, `cargo-nextest`. Hooks: `prek install`.

## License

MIT or Apache-2.0, at your option.
