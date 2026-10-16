# TUI

## TUI

neogit/doom keybindings, every binding configurable. Screens: Status
(the branch band, a rule, then the repo band; stage/unstage/
discard/commit/branch), Log (`l l` HEAD's history, `l b` every branch,
remote branch and tag; both draw `diffler_core::loggraph`'s lanes, one row
per commit so a merge or fork draws its connector on the commit's own row,
HEAD's first-parent line in the accent; `]`/`[` step commits that carry a
name), Diff/review (file sidebar + pane, unified or
`|`-toggled side-by-side; `c` comment, `V` visual select, `r` reply/resolve,
`m` viewed, `y`/`Y` yank feedback as markdown, `e` `$EDITOR` jump). Every motion, the
paging keys included, moves the pane holding the keyboard: `<c-d>` walks the
file list from the sidebar, the cards from the comments pane (counted in
cards, since they are multi-line), and the rows from the diff. `C` opens
the comments sidebar on the right, a third pane the motions walk: its
selection seats the diff cursor on that comment, so the pane's own verbs
(reply, resolve, delete) reach it with no handling of their own. Every
comment but the one under the cursor draws as one line, author then the
body's first line elided; the cursor's own opens the full card, and only
that open card or a group's last item trails a blank spacer, so a busy pane
reads as a dense list rather than a wall of gaps. The author leads each row
in a colour stepped by the golden angle from where that author first
appears in the pane's own order, lifted through `readable_on` for contrast;
the human's own author name and the agent's take the theme's fixed accent
and purple, since a reader looks for those two first and neither hue is
ever handed to anyone else. `t` in that pane cycles its own grouping,
independent of the file sidebar's own layout: by file (diff order), by
author (first-appearance order), by status (open, replied, resolved, the
last folded by default), or a flat list with no headers at all. A
grouping's headers follow the same header/count/fold shape `section_rows`
gives the file sidebar (`group_header_line` the one they share), its items
indented one level under, the way a file indents under its directory:
`tab`/`za` folds the one the cursor sits in, `[`/`]` step headers, and a
header under the cursor selects no comment, so a verb that needs one
declines rather than reaching whatever the diff cursor was last on. A
card sits on a faint surface of its own (`card_surface`) and its text wraps
at the pane's width, by word, the open composer included. A thread draws each
reply as its own block, an author line over the body, with a bar in the
author's colour in place of the card's status bar; the reader's own replies
line up under the first comment and everyone else's indent `REPLY_LANE`
columns. A forge thread is a
flat list of replies, so the lanes show authorship and never nesting. A
closed thread folds the replies between its first comment and its latest
reply into one `▸ N earlier replies` row once it has more than two, and
always shows the latest reply in full; `tab`/`za` on any of its rows
opens it (`DiffView::open_threads`, read through `CardViews` by both the
row build and the drawing, so the two agree; `thread_folds` is the rule). Comments,
replies and edits are written in place: the composer occupies the rows the
finished card will, under the anchored line, at the top of the file for a
whole-file comment, under the thread for a reply. `<c-g>` there, and in the
input modal behind a branch name, a PR title or body, and a review summary,
hands the buffer to `$EDITOR` on a scratch temp file, deleted once the text
is read back whatever the outcome, and reads it into the same box; a
cancelled edit, a failed editor, or the box holding nothing focused all
leave it exactly as it was, the last one saying so. Runs (the
CI run list), Graph (CI run detail on the shared node-graph component), Prs
(open PRs of the repo's forge), CiLog (a
job's log folded into its real steps), and File (below). The diff sidebar has three
layouts (`t` cycles): tree, review (to-review vs a folded viewed bucket,
membership derived from the hash-keyed viewed marks so an edited file falls
back into to-review), and kinds (below), plus walkthrough ([walkthrough.md](walkthrough.md)) where the
review has at least one. Every group header carries the
`+A -B` of what it holds, the file row's own diffstat summed over its files
and right-aligned in the same column, so a folded group still says how big it
is; a header knows its name and not its members, so the sums come from one
pass per frame over the layout on screen. Inside a group, a directory or a
section alike, the files already viewed sort to the top, so what is left to
read is one run at the bottom the way the review layout's buckets do it.
A viewed file leads with a green `✓` in place of its status glyph, the way a
resolved comment does in the comments pane, so a run of finished files reads
down the left edge. The lead cell stays the cursor's own `▌` alone, and a
rail in the diff means an added line and nothing else.
`m` marks the file and moves to the row listed under it, walking what the
sidebar shows: a folded group stays folded, and reaching the end of the list
with files left says so. With nothing below it the cursor holds its row
rather than following the file, which has just sorted to the top of its
group, so marking upward from the bottom keeps the reader where they were.
`[`/`]` step the sidebar's headers, folder to folder or section to section,
the way they step the status screen's groups; every bracket motion, there and
in the diff, walks rows through one `step_to`. On a header, `m` covers everything the header stands
for, a directory's whole subtree or a kind's whole bucket, and a second press
puts it all back. The members come from the grouping, never from the rows, so
a folded Generated marks the files it hides. `u` hunts the next unviewed anywhere, reading
`DiffView::display_order`, the same order with nothing folded. Both read the
sidebar's order, since the diff's file order is a different order on screen.
We ask the terminal for clicks, drags and SGR coordinates only
(`set_mouse_capture`), the modes every multiplexer forwards. A click
selects a row; on the status screen and in the diff file sidebar it also
folds a folder or a group header. A double-click opens what a click
selected. A right-click, or a left press held still for half a second
(`app/menu.rs`), opens a menu of the verbs that fit the row under the
pointer: each entry is a palette `Command`, so it names its key and runs
the same action. The diff screen picks the verbs by the pane the click
lands in (`diff_menu_actions`), the status screen by the row's kind
(`status_menu_actions`), and over an open selection the entries act on the
whole range. Screens with no mouse support open no menu. A click on the tab
row switches tabs, read against the width the row was drawn at. A click
outside an open composer keeps its text aside (`DiffView::parked_drafts`,
one per line, reply or edit) and closes it; opening the composer on the
same target brings the text back.
The status screen keeps the flat magit list. OSC52 clipboard works over
ssh/tmux.
The diff pane folds hunks (`app/diff/folds.rs`), the same unit `]`/`[`
step through. `za`/`<tab>` on any row of a hunk folds it, and on a folded
hunk opens it; `zM` folds every hunk of the file and `zR` opens them all.
Nothing starts folded. A folded hunk replaces its header and everything
under it with one `DiffRow::Fold` that reads as the header plus what it
hides, `@@ -19,7 +19,7 @@ fn f() {  ⋯ 7 lines +1 -1 · 1 comment`, so `]`/`[`
(`DiffRow::is_hunk_header`) land on it folded or open. A fold is keyed by
the hunk's id, so a hunk the agent edits comes back open with the change
showing. An open composer's rows stay on screen inside a folded hunk.
Motions that can land on a hidden line open its hunk through
`DiffView::reveal_line`: `(`/`)`, and a committed search, which matches a
fold row on the code it hides. `RowRef::Hunk` and `RowRef::Fold` each find
the other form of the same hunk, so folding or opening one keeps the
cursor on it.

## Image preview

A binary file named `.png`/`.jpg`/`.jpeg`/`.gif`/`.webp`
(`app::image::is_image`) draws its sides as pictures in the diff pane
(`ui/image_pane.rs`): before and after side by side, stacked below 60
columns, or the one side an add or a delete has, each fitted and centred in
its own frame with its pixel size and byte size under it; whole-file comment
cards keep rows below. `FileDiff::blobs` carries each side's git blob id,
which `sides_hash` folds in and `content_hash` returns as is for the new
side, since a binary file has no text to hash: a rewritten image refreshes,
and a viewed one falls back into to-review, the way an edited text file
does. The draw records what it
wants as an `ImageKey` (path, blob ids, fit size) and `queue_image_preview`
turns a key that is neither on screen nor in flight into a worker request:
`Review::compute_binary_sides` reads each side by blob (`Vcs::read_blob`),
the worktree side from disk when the store holds no object for it, capped
at `MAX_PREVIEW_BYTES` a side, then `app::image::build_preview` decodes it
and encodes it for the terminal at that size, scaling a small image up with
nearest-neighbour so its pixels stay readable. The protocol comes from
ratatui-image's `Picker::from_query_stdio`, asked once in `run` after raw
mode and before the event pump reads stdin: kitty (Unicode placeholders,
which survive a multiplexer), sixel or iTerm2 when the terminal answers,
halfblocks otherwise.

## Symbol lens

`*` in the diff pane (`app/diff/lens.rs`, over the pure
model in `diffler_core::lens`) names every identifier on the cursor line,
from one parse per side (`LanguageRegistry::symbols`, plain words where a
language has no grammar), up to nine, tints each one's uses in its own golden-angle colour
through the same `Mark` path search hits take, and draws each name's digit
in red over its first letter on that line, so the number that picks a name
sits on the name. A name the parse treats as an item on that line (a
definition, a call or method call the tags query marks, or a type position)
reaches every file of the diff, both sides, whether or not the diff defines
it; any other name is a local and reaches only its enclosing function
(`ScopeIndex::enclosing`, held to that one function's span even when the
file defines another of the same name), or its file at the top level. Uses count only on
lines the diff shows, a deleted line on its old side and an added or context
line on its new side, so a context line counts once. Building it parses both
sides of every file, so it is a worker like enrichment (`pending_lens` →
`AppEvent::Lens`, its token held by the view that asked, so an answer for a
replaced view is dropped). `*` again cycles the focus through the
names and back, `#` the same ring backwards (vim's pair), a digit picks one the way a
numbered list does, and `esc` closes it. A focused name opens the references sidebar in the comments
sidebar's place (`Pane::References`; opening either puts the other away): its
uses in diff order, one per line, grouped by file under a header that dims
the folder and cuts it from the front so the file name always shows, each a
few lines of the hunk around it with the diff's `-`/`+` markers, their shared
indent cut, highlighted from the file's enrichment (queued for every listed
file) and the name tinted. A reference is keyed by its path, side
and line number and found again when it is seated, so expanding context or
re-diffing never points it at the wrong row. The keyboard stays in the diff when it
opens; `l` moves into it, and moving its selection seats the diff on that use
through `seat_ref`, switching file and opening a fold as needed, the way the
comments sidebar's selection drives the diff cursor; paging, `[`/`]` (file to
file), the wheel and a click work there as they do in the comments sidebar. `n`/`N` step that same
selection when a name is focused, and walk every tinted use in diff order when
none is. Side-by-side draws no lens and `*` there names the unified view. A
refresh that moves the model drops the lens, since its line numbers name the
old text.

## Kinds sidebar

`classify::Rules` buckets a path into one fixed set,
Source / Tests / Docs / Config / Build & CI / Generated / Assets / Other:
the reader's `[classify]` globs, then what the repo declares, then the
built-in table. The table's order is the design, Generated ahead of Tests so
a generated fixture reads as noise, and every rule reads the path alone, so
a row build costs no IO. Buckets with nothing in them contribute no header,
Generated and Assets start folded, and both grouped layouts share one
`BTreeSet<Bucket>` of folds and one `section_rows` emitter (depth 0 header,
depth 1 file, which the renderer's indent reads). What the repo declares is
`linguist-generated`/`-vendored`/`-documentation`, read through
`Vcs::attr` with the mapping in `classify::declared`, so the backend stays
free of sidebar policy. That lookup walks the attribute files per path
(~75µs each, measured), so it is a worker like any other read: `queue_declared`
on open, on `t`, and after a refresh that moved the file list, answering as
`AppEvent::DeclaredKinds` with a token that drops an answer for a list the
view has replaced. Until it lands the sidebar groups by the table alone.

## File view and blame

`Vcs::blame` returns line runs, one per commit,
remapped onto the worktree buffer so an edited file attributes its committed
lines correctly and its new ones to nobody. `Review::compute_file` opens its
own backend like `compute_refresh`, so the read, the blame and the highlight
all run on the blocking pool and the screen opens rendered. One screen serves
both jobs: `Screen::File` is the file viewer, and `b` toggles its blame
column, because a viewer and a blame view differ by one column. `]`/`[` step
commit runs, `<cr>` reviews the commit that wrote the cursor line. The gutter
prints a commit only on the first line of its run. Reached with `B` on the
file under the cursor (status or diff), or `gf`, the fuzzy picker over
`Vcs::tracked_files`: the diff screens list only changed files, so the picker
is the one way to a file the review does not touch, and it also sends one
straight to `$EDITOR`. `y` copies the selected lines' text; `Y` copies the
forge's link to them (`ci::blob_url`, GitHub/GitLab/Forgejo line-range
anchors) and `gx` opens that link in the default browser. The link names the
branch when it is pushed with nothing ahead, else the remote's default
branch, since a link to commits the forge lacks would 404.

## Status bands

The branch band leads with the branch's own PR when it has
one, since that is what the branch is for, then the repo's walkthroughs
when it has any (a header counting them, one row per walkthrough, folded
like any other group), then the working-tree sections,
Unpushed (commits no remote-tracking ref contains, walked to `UNPUSHED_LIMIT`
and counted `N+` at the ceiling), and Recent commits. A bare
rule then opens the repo band: Branches, Open pull requests (fetched the
first time the group unfolds), CI runs. A branch listing resolves no
upstream: one costs a config read and a graph walk (~2ms measured, so 570
branches cost 1.5s on every refresh), and only the rows the section renders
ask `Vcs::divergence` for theirs. A group is present when the repo can
have the thing at all, so zero is an answer and only a repo without remotes
loses its Unpushed section. `[`/`]` step group headers, `tab` folds one;
a commit carrying CI runs takes a `▸` between its glyph and sha and unfolds
them beneath it. `y` copies whatever the cursor addresses in the form you
would paste: a pull request as its forge URL, a commit as its full sha, a file
or a folder as its repo-relative path (a hunk header and a line inside an
expanded diff both address their file, the way the editor jump reads them);
over a `V` selection it copies every row the selection covers, one per line,
each value once, and the branch-checkout key on a listed pull request checks that one out. Every
async arrival (CI poll, PR fetch, watcher refresh)
re-seats the cursor through `status_cursor_anchor`, keyed by identity
(path, oid, branch name) rather than row index.

## Language breakdown

`language::of_path` names a path's language, reusing
the highlighter's extension table (`syntax::registry`) so the languages
diffler can parse are mapped in one place, with a small table beside it for
the ones it counts without highlighting. Colours are Linguist's own hexes,
the ones a repository page uses, lifted by `readable_on` until they clear a
3:1 contrast with the theme's background: `#292929` JSON is invisible on a
dark terminal and Linguist tuned that palette for a white page. Two surfaces
read it. The status head band's `Languages` line breaks the working tree's
churn down per language, from the diffstats the screen already sums, and
stays hidden below two languages. `L` opens the Stats screen, a table of
files/lines/code/comments/blanks per language that `stats::scan` fills from
a read per tracked-or-untracked file on the blocking pool, token-guarded like
every other worker; `s` cycles the sort, `<c-r>` counts again. The scan
leaves out what `classify` calls Generated, lockfiles included, the way a
repository page does and `scc` does by default, and says at the bottom what
it left out. Comment counting is a per-language token table, not a parse: a
line opening with a comment token is a comment, a shebang is code.
