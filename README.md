# diffler

**magit, without Emacs.** diffler is a terminal git UI that stages, commits and
reviews pull requests, and lets your coding agent reply to the comments you
leave in the diff.

[![crates.io](https://img.shields.io/crates/v/diffler.svg)](https://crates.io/crates/diffler)
[![npm](https://img.shields.io/npm/v/@mattfillipe/diffler.svg)](https://www.npmjs.com/package/@mattfillipe/diffler)
[![IRC](https://img.shields.io/badge/IRC-chat.h4ks.com-blue.svg)](https://chat.h4ks.com)

![diffler: reviewing an agent's edits as it makes them, a walkthrough of the change, and a pull request review](assets/demo.gif)

I love magit and wanted it as its own program, with the vim-style keys it has
in [Doom Emacs](https://github.com/doomemacs/doomemacs). Then coding agents
came along, and reading what they write became a big part of the day. So in
diffler you comment right in the diff, and your agent replies in the thread
and fixes the code while you watch.

## Features

- Stage, commit, branch, push and pull with vim-style keys
- Comment on any line or range, in a diff or in any file of the repo
- Review GitHub, GitLab and Forgejo pull requests without checking them out
- Follow CI runs and read their logs
- Your agent answers your comments and walks you through its changes, if you want it to
- Step back through a file's history and see who changed each line
- Open several projects and worktrees side by side as tabs
- Works in git and jj repos
- Shows images right in the terminal, over [kitty, sixel or iTerm2](https://github.com/benjajaja/ratatui-image)

## Install

On macOS or Linux:

```sh
brew tap matheusfillipe/diffler https://github.com/matheusfillipe/diffler
brew install diffler
```

On Windows:

```sh
winget install matheusfillipe.diffler
```

With Rust, on any of them:

```sh
cargo install diffler
```

Prebuilt binaries for x86_64 and arm64 are on the
[releases page](https://github.com/matheusfillipe/diffler/releases).

<details>
<summary><b>Scoop</b></summary>

```sh
scoop bucket add diffler https://github.com/matheusfillipe/diffler
scoop install diffler
```

</details>

<details>
<summary><b>Arch</b></summary>

`diffler-bin` on the AUR ships the prebuilt binary, so it installs without
building.

```sh
yay -S diffler-bin
```

</details>

<details>
<summary><b>Nix</b></summary>

The flake serves the prebuilt binary. `nix run` tries it without installing
anything, `nix profile install` keeps it.

```sh
nix run github:matheusfillipe/diffler
nix profile install github:matheusfillipe/diffler
```

</details>

<details>
<summary><b>npm</b></summary>

A wrapper that fetches the prebuilt binary. The package name is scoped, the
command it installs is plain `diffler`.

```sh
npx @mattfillipe/diffler
npm install -g @mattfillipe/diffler
```

</details>

## Quick start

```sh
diffler                 # open it in the current repo
diffler path/to/repo    # or in another one
```

You land on the status screen, which lists your changed files, unpushed
commits, branches and pull requests. Keys are written vim-style: `<cr>` is
Enter, `<c-d>` is Ctrl+d, and `b` `p` means press `b`, then `p`.

Move to a file and press `<cr>` to see its diff. `s` stages it, `cc` commits,
and `c` leaves a comment on the line under the cursor.

For pull requests, press `b` `p` to list the open ones and `<cr>` to open one's
page with its description and conversation. There, `c` comments on the pull
request and `R` requests a reviewer. Press `<cr>` again to review its diff, and
`S` to submit your line comments as one review. `b` `P` opens a new pull
request from the current branch.

### With a coding agent

diffler talks to your agent over MCP, the protocol agents use to call outside
tools. Connect it once:

<details>
<summary><b>Claude Code</b></summary>

Install the plugin. It connects diffler and adds three commands: `/df` to
answer your comments, `/dfa` to walk you through a change, and `/dfr` to review
a change and leave comments.

```sh
claude plugin marketplace add matheusfillipe/diffler
claude plugin install diffler@diffler
```

If you only want the connection, without the commands:

```sh
claude mcp add diffler -- npx -y diffler-mcp
```

</details>

<details>
<summary><b>opencode</b></summary>

Add the server to `opencode.json` in the project, or globally in
`~/.config/opencode/opencode.json`:

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

And install the `/df`, `/dfa` and `/dfr` commands (opencode has no package
mechanism for commands, so this fetches the ones maintained in this repo):

```sh
mkdir -p ~/.config/opencode/commands
for c in df dfa dfr; do curl -fsSLo ~/.config/opencode/commands/$c.md \
  https://raw.githubusercontent.com/matheusfillipe/diffler/main/.opencode/commands/$c.md; done
```

</details>

<details>
<summary><b>Any other MCP agent</b></summary>

Run `npx -y diffler-mcp` as a stdio server. It finds the port of the diffler
running in your repo, since each diffler writes its own to `.diffler/mcp.json`.
Or point it at `http://127.0.0.1:8417/mcp` (streamable HTTP), the port the
first diffler you started takes. The server also ships `review`, `walkthrough` and
`critique` prompts that prompt-aware clients surface as commands.

</details>

Now comment on the agent's changes in diffler and press `Z`. That tells your
agent your comments are ready. It replies in each thread and fixes the code,
and you resolve the thread once you're happy. You can also ask it for a
walkthrough of its change and open that from the status screen. More in
[docs/walkthroughs.md](docs/walkthroughs.md), and the agent's tools are listed
in [docs/mcp.md](docs/mcp.md).

If you don't use an agent, turn the connection off with `diffler --no-mcp`, or
`enabled = false` under `[mcp]` in the config.

## How it compares

| | diffler | [tuicr](https://github.com/agavra/tuicr) | [lazygit](https://github.com/jesseduffield/lazygit) | [magit](https://magit.vc) |
|---|:---:|:---:|:---:|:---:|
| Standalone binary | ✅ | ✅ | ✅ | ❌ |
| Stage and commit | ✅ | ❌ | ✅ | ✅ |
| Line comments | ✅ | ✅ | ❌ | ❌ |
| PR review | ✅¹ | ✅² | ❌ | ❌ |
| Agent replies in the thread | ✅ | ❌³ | ❌ | ❌ |
| Agent walkthroughs | ✅ | ❌ | ❌ | ❌ |
| jj | ✅ | ✅ | ❌ | ❌ |

<sub>¹ GitHub, GitLab and Forgejo.</sub><br>
<sub>² GitHub, GitLab, Gitea, Bitbucket, Azure DevOps and Gerrit.</sub><br>
<sub>³ tuicr exports your comments as markdown for you to paste into the agent.</sub>

## Keys

You move around like in vim: `j`/`k`, `gg`/`G`, `/` to search, `<c-d>`/`<c-u>`
to scroll. If you forget a key, `?` shows every key of the screen you're on,
and `<c-k>` lets you search all actions by name.

| Key | What it does |
| --- | --- |
| `<cr>` | Open what's under the cursor |
| `s` / `u` | Stage / unstage |
| `cc` | Commit |
| `c` | Comment on the line, or on a range you selected with `V`, in a diff or any file |
| `Z` | Send your comments to the agent |
| `m` | Mark the file as viewed |
| `t` | Change how the file list is grouped |
| `\|` | Switch between a unified and a side-by-side diff |
| `]` / `[` | Jump to the next / previous hunk |
| `za` / `tab` | Fold or unfold a hunk, or the earlier replies of a thread |
| `*` / `#` | Find where the symbol on the line is used |
| `d` | Review against another branch or commit |
| `C` | Open the list of comments |
| `gf` | Open any file in the repo |
| `alt-n` | Open another project as a tab, then `alt-h` / `alt-l` to switch |
| `w` | Open, create or remove a worktree |
| `y` / `Y` | Copy your selection or this file's comments / all comments, to paste into any agent |
| `gy` / `gx` | Copy / open the GitHub, GitLab or Forgejo link to the line |
| `B` | See who last changed each line |
| `{` / `}` | Step to the file's older / newer version |
| `e` | Open the file in your editor |
| `q` | Go back, or quit |

Every binding is remappable in [docs/config.example.toml](docs/config.example.toml).
The mouse works, over tmux too: click to select or fold, and
right-click (or press and hold) a line, file or comment for what you can do
with it.

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
[gallery](showcase/THEMES.md). Add your own as a TOML file in
`~/.config/diffler/themes/`: [docs/themes](docs/themes/README.md) lists
every key.

![the review screen in catppuccin-mocha: the file tree, a diff carrying a comment thread with the agent's reply, and the comments sidebar](showcase/img/catppuccin-mocha.png)

## Development

```sh
just ci     # fmt + clippy + tests
just e2e    # PTY end-to-end suite (needs uv)
```

Requires Rust 1.90+, `just`, `cargo-nextest`. Hooks: `prek install`. See
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT or Apache-2.0, at your option.
