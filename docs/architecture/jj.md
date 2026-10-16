# jj backend

Colocated repos, a `.jj` directory beside `.git` (what
`jj git init` makes by default), and repos with no `.git`. In a colocated repo git's HEAD sits on
jj's `@-` and the worktree matches `@`, so `JjVcs` delegates every read
(diff, log, blame, `read_at`, tree diffs) to `GitVcs` unchanged. Writes
shell out to `jj`: commit -> `jj commit`, extend -> `jj squash -u` (a bare
squash opens an editor when both sides carry a description), amend ->
`jj squash` with the message, reword -> `jj describe -r @-` (touches only
`@-`'s description, never the working copy), branch create/delete ->
`jj bookmark create -r @` / `jj bookmark delete exact:`, checkout ->
`jj new <rev>` (checkout keeps working on top of a branch; `jj edit`
edits its tip commit in place), discard -> `jj restore
root-file:<path>`. `c c` over a `V` selection of working-copy rows
commits just those files and folders, a rename's old path included
(`Vcs::commit_paths`, `jj commit -- root:<path>…`); a selection holding
no file row commits nothing, and git's `c c` commits the index and
ignores a selection.
A message travels as `--message=<text>`; a name jj
would otherwise resolve as a revset or fileset expression (a checkout's
target, a deleted bookmark, a discarded path, a commit id) travels as a
quoted jj string literal, after `--` where it is positional, since a
leading `-` reads as a flag and git
allows names (`fix(x)`, `u@v`) that parse as fileset or revset syntax
there. A created bookmark's name is never resolved as an expression, so
it goes through unquoted. Every call sets `JJ_EDITOR=false`; a prompt jj
opens anyway then fails at once, keeping the write's UI thread
responsive. Staging, unstaging, hunk staging, and stash have no jj
equivalent; each returns a `VcsError::Rejected`, and the UI shows it as a
status message (stash's names the change menu's New). The change menu
(`TransientKind::Change`: `C` on the status screen for `@`, `c` on the log
for the commit under the cursor) runs `Vcs::change` with a `ChangeOp`:
new -> `jj new`, edit -> `jj edit` (log only; `@` already is the working
copy), describe -> `jj describe` (the input opens on the full
description, read with git2 from the commit the view last saw:
`ChangeInfo::commit_id` for `@`; an empty one clears it), squash -> `jj
squash --revision`, passing `--message=<parent>\n\n<change>` when both
are described, since jj would open an editor to join them, abandon
(after a confirm that says `@`'s edits go with it) -> `jj abandon`, and
undo/redo -> `jj undo`/`jj redo`. jj snapshots edits before undo or redo
and would then act on that snapshot, dropping them, so `jj util
snapshot` runs first and the verb is refused while a snapshot is the
newest operation. `@` goes to jj as `@`, since its commit id moves with
every snapshot; a log row's commit id goes as `change_id(<its change>)`,
so a commit rewritten since names its current version and never comes
back as a divergent copy. jj refuses an immutable commit itself and we
show its error. A git repo leaves the menu's prefix unbound and lists it
nowhere, and its backend declines `ChangeOp` with `vcs::JJ_ONLY`.
Network ops run through `network_argv` in the git-op worker, which reads
what a push sends fresh, so a bookmark made a moment ago counts; `P u`
carries the `NetworkOp` through its confirm and builds the argv after.
Fetch runs `jj git fetch` (or `--all-remotes`). Push takes the nearest
local bookmark (`heads(::@ & bookmarks())`): on `@` or `@-` it runs `jj
git push --bookmark=exact:<name>`; further back and still mutable it is
refused, asking for `jj bookmark move`; otherwise, with `@-` mutable, it
runs `jj git push --change=@-`, which names a bookmark after the change,
and with `@-` immutable it is refused as nothing to push. jj tracks the
remote bookmark a push makes. `P u` adds `--remote=<picked>`; with no
flag jj picks `git.push`, else `origin`. Pull reads `Vcs::pull_target`
(the nearest local bookmark and the remote it tracks, `origin` first)
into `App::pending_jj_pull`, runs `jj git fetch --remote=<that remote>`,
then `Vcs::rebase_onto_remote` on the UI thread: when that remote
bookmark is no longer in `::@`, `jj rebase --branch=@
--onto=<remote bookmark>`, warning about rebased changes that now
conflict and a local bookmark that moved on both sides; a bookmark the
remote deleted leaves `@` in place and says so. A jj network op's toast
shows jj's `Error:` line, or its first line joined with the next when it
only introduces a list, and skips git's push and pull recovery dialogs.
A PR checkout fetches the
head ref with git and switches through `Vcs::checkout`; creating a PR
asks for the bookmark to be pushed first. `Vcs::status` reads
`working_tree_diff` (one diff pass, `@-` against the whole working copy)
into the `staged` section: jj's colocation
snapshot marks a new file intent-to-add in the git index, so merging
git's own untracked/unstaged/staged lists would double-count it, git2
reporting it once as added (tree vs index) and again as modified (index
vs workdir). The status screen folds to that one section
(titled "Working copy (@)") and its hint line drops `s stage`; nothing else
about the screen forks for jj, but its head line. A repo with no `.git`
(`jj git init --no-colocate`) works too: `repo::discover` falls back to
the nearest `.jj`, and `JjVcs` opens jj's own store (`.jj/repo/store/git`,
or wherever `git_target` points) with git2 as a bare repo whose workdir is
the root. That store has no index, its HEAD never moves and jj never
exports bookmarks into it, so we never write to it: `GitVcs` carries a
base (`set_base`) that every read of HEAD goes through (`head`,
`head_tree`, `unpushed`, the log walk, `head_message`, and a `HEAD`,
`HEAD~n` or `HEAD^` in `resolve`, `read_at`, `commits_between` and
`blame`), set to the first parent of `@` (unborn on jj's root commit). A
colocated repo leaves it following git's own HEAD, which jj keeps on that
same commit. The working copy diffs with `diff_tree_to_workdir`, ignores
`.jj/`, reports a new file as added (jj tracks it), and lists
`tracked_files` from the base's tree. A secondary workspace (`.jj/repo` a
file) is refused with a message naming the main one. jj's own knowledge
is a `JjView` (`@`'s change id, description and bookmark, else the
nearest bookmarked ancestor's, local before `name@remote`; the base; every
bookmark in a repo with no `.git`; short change ids over twice the log's
depth of `::(@ | bookmarks() | remote_bookmarks() | tags())`), read in two
`jj log --ignore-working-copy` runs by `Vcs::read_jj_view`. Only
`App::new` and `Review::compute_refresh` (on the blocking pool, first, so
the worker's handle reads HEAD as the base) call it. `App::new` runs on
the UI thread when a project opens, at startup or as a new tab, and a repo
with no `.git` pays 3 `jj` runs there: `Review::open`'s state read, then
the view; the refresh carries the view, HEAD and the commit lists in
`Refreshed`, and `apply_refresh` assigns them and hands the view to the
UI's handle through `adopt_jj_view`, which runs nothing. Every other read
stays a git2 read: `log`, `unpushed` and `commits_between` attach change
ids, jj's `(no description set)` for an empty subject, and (with no
`.git`) the bookmarks as ref labels from the adopted view, dropping the
`jj/keep/*` refs; with no `.git`, `branches`, `all_branches` (`name` and
`name@remote`) and `divergence` read the view's bookmarks too. A handle
that has adopted no view asks jj for the state alone once, before its
first read of HEAD, which is how a worker's fresh backend finds `@-`, and
a write asks again for the commit it made, failing when `jj` fails. A
colocated repo whose `jj` fails or is missing keeps its git2 reads: the
refresh carries the error, shown once. A repo with no `.git` keeps the
UI's last good base, and a refresh whose handle never learned one fails
with jj's error. A colocated repo resolves a revision through git first
(refs, `main~2`, oids, `HEAD` as the base), since git's refs are current
where jj's import of them can lag. A repo with no `.git` resolves a
leading bookmark name through the view, since its store's refs are stale
or absent, and applies a `~n`/`^n` suffix with git2. What git cannot
read (`trunk()`, change ids), and `@` with its relatives, which git
reads as HEAD, goes to `jj log -r <rev>`, refusing a revset that matches
no commit, several, or jj's root commit; a git ancestry suffix on a name
nothing knows is refused, never handed to jj, which reads `~` as a set
difference. A pull request fetch declines in a repo with no `.git`,
since `git fetch` would reach an enclosing repo. The watcher ignores
`.jj/` the way it ignores `.git/objects`, except `.jj/repo/op_heads`:
every jj operation lands there, and a repo with no `.git` has no other
signal.
