# Themes

A theme is a TOML file in `~/.config/diffler/themes/` (or
`$XDG_CONFIG_HOME/diffler/themes/`). Its file name is its name, so
`themes/midnight.toml` is the theme `midnight`. Pick it with `T`, set
`ui.theme = "midnight"` in your config, or run `diffler --theme midnight`.

Every key is optional. A theme starts from a built-in and changes only the
colours it sets:

```toml
extends = "dracula"
accent = "#ff8800"
```

[example.toml](example.toml) sets every key, as a starting point for a full
theme.

## Keys

`extends` is the built-in the theme starts from, `github-dark` when left out.
`syntax` is the built-in whose code-highlight palette the theme uses, the one
`extends` names when left out. Both take a built-in name: `github-dark`,
`github-light`, `catppuccin-mocha`, `tokyo-night`, `gruvbox-dark`, `nord`,
`rose-pine`, `kanagawa`, `dracula`.

Every colour is `"#rrggbb"`.

| Key | What it colours |
|---|---|
| `bg` | the screen background |
| `panel` | popups, menus and the run graph's status bar |
| `border` | pane and popup borders |
| `cursor_line` | the row under the cursor |
| `fg` | text |
| `dim` | secondary text: hints, paths, timestamps |
| `accent` | headings, focus, and your own comments |
| `purple` | the agent's comments |
| `del_line_bg` / `add_line_bg` | removed and added lines |
| `del_emph_bg` / `add_emph_bg` | the words that changed inside those lines |
| `annotated` | lines a comment is anchored to |
| `search` / `search_current` | search matches, and the one under the cursor |
| `added` | `+N` counts (`-N` counts use `error_fg`) |
| `warn_fg` / `error_fg` | warnings and errors in the status bar |
| `chip_fg` / `chip_bg` | the screen name at the left of the status bar, `bg` on `accent` when left out |

A file diffler cannot read (bad TOML, an unknown key, a colour that is not
`#rrggbb`, an unknown `extends`) opens as `github-dark` at startup, or keeps
the current theme when picked with `T`, and the status bar names the file and
the problem.
