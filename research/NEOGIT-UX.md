# Neogit UX Reference (extracted 2026-06-11, clone at /tmp/neogit-ref)

The UX model diffler's TUI follows. Magit-style transient popups + vim keys.

## Status buffer

Section order: Hint line → Head (branch, upstream, push remote) → sequencer states
(merge/rebase/cherry-pick, when active) → Untracked → Unstaged changes → Staged changes →
Stashes (folded) → Unpulled/Unmerged (folded) → Recent Commits (10, folded).

File row: `[mode_text padded] [filename] [mode change] [submodule marker]` — mode_text =
"modified" / "new file" / "deleted" / "renamed".

TAB folds section/file. Files expand inline to full diff: file header → hunk headers
(`@@ -a,b +c,d @@ ctx`) → +/- lines. `{`/`}` jump hunk boundaries. Cursor position preserved
per-section across refresh; empty lines skipped on j/k.

## Core keys (status)

```
j/k move        TAB/za toggle fold     1/2/3/4 fold depth    q close
s stage (file|hunk|selection)    S stage all unstaged    <c-s> stage everything
u unstage       U unstage all          x discard (confirm)   - reverse staged
<cr> go to file/commit           <c-r> refresh             { } hunk jumps
<c-n>/<c-p> next/prev section    Y yank OID                ? help popup
```

## Popup model (transient)

Single key opens popup; single key inside runs action; switches (-a etc.) toggle first.
Popups: `c` Commit, `b` Branch, `l` Log, `p` Pull, `P` Push, `d` Diff, `Z` Stash, `X` Reset,
`t` Tag, `f` Fetch, `m` Merge, `r` Rebase, `v` Revert, `?` Help. Flags persist across runs.

### Commit popup (`c`)
Actions: `c` commit (opens editor), `e` extend, `a` amend, `w` reword, `f` fixup, `s` squash.
Switches: `-a` all, `-e` allow-empty, `-v` verbose, `-h` no-verify.
Flow `cc`: editor opens gitcommit buffer; submit `<c-c><c-c>`, abort `<c-c><c-k>` or `q`.
If nothing staged: prompts to stage all.

### Branch popup (`b`)
Checkout: `b` branch/revision, `l` local, `c` new branch (create+checkout).
Create: `n` new branch (no checkout). Do: `m` rename, `X` reset, `D` delete.

### Log popup (`l`)
`l l` = log current branch. Line format: `[7-char oid] [decorations] [subject]`, optional
graph. `<cr>` opens commit view. Switches: -n max-count (256), --graph default on,
--decorate default on.

## Staging granularity

File-level `s` stages file; hunk-level `s` (cursor on hunk) applies patch `--cached`;
visual `V` + `s` stages selected lines as patch. Same for `u`/`x` reversed.

## Visual mode

`V` line-select across files/hunks; `s`/`u`/`x`/`-` apply to selection; popups receive
selection as context (e.g. select commits → `b` branch popup pre-filled).

## Defaults worth copying

word_diff_highlight=true, recent_commit_count=10, graph_style=ascii, hint line on,
sections untracked/unstaged/staged open + stashes/recent folded, confirm on discard.

## Diffler mapping decisions (M1)

- Keep: j/k, TAB folds, s/u/x/S/U, cc commit flow ($EDITOR), b popup (c new+checkout,
  n new, D delete), ll log, {/} hunks, <cr> open, <c-r> refresh, q, ? help, Y yank.
- Diffler-specific: `c` on a diff line in review context = comment (popup `c` stays commit
  on status; in diff/review view `c` = comment, commit popup reachable from status only).
  `V` visual select → `c` comment on range. `v` mark file viewed (GitHub-style; resets when
  file content hash changes). `y` copy feedback markdown (file), `Y` copy all feedback.
- Not in M1: pull/push/fetch/stash/rebase/merge/tag popups, reflog, submodules.
