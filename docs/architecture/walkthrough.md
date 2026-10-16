# Walkthrough

The agent that made a change is the only party who knows the
order it should be read in, and a walkthrough is that order: one stop per
real decision, as few as the change needs, opened by its own summary.
**A walkthrough is a review source of its own**, `ReviewSource::Walkthrough
{ id }`, key `walkthrough-<id>`: its own comments, its own viewed and seen
marks, stored at `.diffler/reviews/walkthrough-<id>.json`, nothing shared
with the working tree, a PR, a commit or a range review. **A stop is an
agent comment with an order and a title.** `Comment` carries `title` and
`anchor_ref` (the agent's `path#symbol` / `path:a-b` / `path`, kept so the
worker can resolve it again after the code moves), and `Walkthrough` is
`{ id, title, author, at, skipped, stops: Vec<String>, summary:
Option<String>, rev: Option<String>, about: ReviewSource }`: `stops` is the
primary comment ids in reading order, `summary` is the walkthrough's own
overview, a markdown body exactly like a stop's but with no comment or
anchor behind it, `None` for a walkthrough with none, `rev` is the full oid
of `HEAD` at publish time, `None` for a walkthrough saved before that field
existed, and `about` is the review this walkthrough describes: the working
tree, or the commit/range/PR the human had open when it was published,
defaulting to the working tree for a walkthrough saved before this field
existed. A walkthrough source's session holds exactly one
(`Session::walkthrough: Option<Walkthrough>`); since the source is the
walkthrough's own, every comment in that session is this walkthrough's, its
stops, their notes, and any human reply, with nothing left to track
ownership of. `publish_walkthrough`
(`diffler_core::walkthrough`) targets the source `id` names, or a fresh
random id when it is omitted. That is what gives a stop a thread the human
answers in, a row in the comments pane, and the card renderer, with no
second comment system beside the first. `about` is whichever review the
human has open at publish time (what `review_status` already reports), so
no separate argument names it; revising a walkthrough while looking at its
own diff keeps whatever it already described, since that source names no
review of its own to fall back on. `publish_walkthrough` materialises
each stop as a comment (`author: agent`, `anchor.file` from the ref, or with
no ref the first stop's file that has one, else the first file of the
review it is about)
and each of its `notes` as a further comment in the same file, titleless,
anchored where it names or else at the region's first line; a revision that
passes a stop's or a note's `id` back keeps that comment and its replies,
and every other agent comment the source held goes with it (a human
comment or reply is never one of these, so it always survives). `summary`
is stored on the `Walkthrough` itself, not as a comment, so a revision keeps
or drops it by what the call passes, with nothing to pass back by id.
The status screen's branch band carries a Walkthroughs group
under the branch's pull request: a header named and counted like any other
group header (`Walkthroughs (N)`), folded by
default; unfolded, it lists one row per walkthrough source on disk, newest
published first (`status::load_walkthroughs`, read once per refresh through
the store and again after any change to a walkthrough's own file: publish,
delete, a stop removed), its
title then dimmed ` · N stops` and, once every stop of it is seen, a dim
`✓`. `<cr>` on a row opens that source's diff (`App::open_walkthrough`) in
its walkthrough layout, seated on its summary when it has one, else its
first stop; `<cr>` on the header does nothing special, like every other
header (only `tab` folds it).
The diff sidebar's walkthrough layout (`t` cycles into it only on a
walkthrough's own source, since every other source has none) shows the
open source's own walkthrough: `DiffView::active_walkthrough` reads
`session.walkthrough` directly, so there is nothing to pick between. Opening
a walkthrough by id (`App::open_walkthrough_diff`) resolves `about`
(`App::walkthrough_about`) and installs its own source's diff over that
review's own model (`App::source_model`, which recurses into `about` for a
`Walkthrough` source), the working tree's when `about` is one, even over a
clean working tree (the one caller `install_diff_view` lets through empty),
since the walkthrough's own context files fill the pane once anchors
resolve; leaving the layout with `t` when the diff itself carries nothing
cycles back to it rather than to tree/review/kinds, which would list
nothing. The layout
lists a leading `Summary` row (`TreeNode::WalkthroughSummary`) only where
the walkthrough has one, then one row per stop, no numbers and no group
headers, its title with the file dimmed after it and a ` · N` count once
its region holds more than one comment; the pane heading carries the
walkthrough's name instead of `Files`. A stop is a slide: its region (the
primary comment's anchored span) and every comment anchored inside it, the
agent's and the human's. This layout always windows to the slide on screen
and never falls back to the whole file: `DiffView::slide` names a
`Slide::Stop`, a `Slide::AdHoc` holding a comment no slide's region covers,
or `Slide::Summary`, the leading row's own slide. Reaching a comment by any
route, the comments pane, `]`/`[`, `C`'s own selection, a search hit,
enters its slide first (`enter_slide_for_comment`): the slide it is the
primary of, else the slide whose region contains its line in the same
file, else an ad hoc slide; the sidebar cursor follows when a slide
matched. No comment is ever anchored to the summary, so this route never
lands on it; `]`/`[` (`walk_slide_comments`) reach it only by stepping
back off the first stop, and forward from it land on that same first stop,
since the summary sits before every entry the walk carries. A comment
reached this way always belongs to the open source's own walkthrough,
since that source carries no other. `]`/`[`
walk every comment in slide order, switching slides at the boundary
between two, with whatever no region holds reached last as ad hoc
slides. Selecting a stop row seats the reader through the same `seat_on` a
comment jump uses: the comment's file,
the cursor on the first row of its span, the whole span banded in
`blend(bg, accent, 25)` by `band_referenced`, the one helper the diff pane
and the file view share. The pane shows that region and nothing else of the
file: the rows the primary's `anchor.line..=anchor.line_end` cover, the
hunk header they sit under, every comment anchored inside it, and the open
composer. A primary with no line shows the cards alone; selecting the
summary row (`seat_summary`) shows the same shape with no primary at all,
its own card and no code rows, since nothing is anchored to it either.
`r` on a card answers that stop or note in its thread, which is how the
human talks back; the summary carries no thread, so nothing answers it.
An anchor is `path#symbol` (resolved through `ScopeIndex`, so it survives
the symbol moving), `path:start-end`, `path:line`, or a bare
path, and every resolved one is an inclusive row span: a symbol covers its
whole definition through `ScopeIndex::def_span`, a range clamps to the file,
a C/C++ method defined outside its class answers to `Class::method` too,
a bare line covers itself. Resolution writes `anchor.line`, `anchor.line_end`
and `anchor.line_text` onto the comment, so outdated detection and card
placement are the ones every comment already gets; a `Whole` anchor leaves
the lines unset (a file-level card). A `Lost` or `FileMissing` result marks
the card `stale` and clears its lines the same way, and `DiffView`
remembers which of the two happened per comment id
(`unresolved_anchors: HashMap<String, Located>`): the card names the
difference in one dim `CommentLine::Note` line under the body, `Lost` for a
file that is there but has lost the anchor and `FileMissing` for one gone
from wherever the worker read. A stop or note whose file never turns up in
the model still gets an (empty) `context_files` entry for it, so its card
always has a file to seat on and a slide never renders empty even when
nothing under it resolves.
Bodies render through `app::markdown`, the same
parser every comment body uses, so headings, tables, lists and fenced code
all work; a ` ```mermaid ` or ` ```callstack ` fence becomes a figure
instead, static here and drawn once per frame into the card's rows, which
every agent comment gains and not only a stop. A figure is a
`graph::Drawing`: `Graph`, a navigable flowchart, or `Text`, a sequence
diagram (`graph::sequence`) or callstack tree (`graph::callstack`) laid out
once at parse time into rows of styled text (`graph::text_figure`).
`graph::figure` picks between them: a ` ```callstack ` fence is always a
callstack; a ` ```mermaid ` fence is a sequence diagram when its first line
names a `sequenceDiagram`, else a flowchart through `graph::mermaid` (the
`flowchart` subset the layered engine can draw, simplified where it cannot,
since an agent that gets a rejection it cannot fix is worse off than a
reader looking at a box where a `((circle))` was). A text figure lays out to the
card's width, so a resize re-lays it out through the same width-keyed
cache: a sequence diagram widens the gap before each message's right end
until its label fits, left to right, while the width lasts, and elides the
label to its lane after that; a callstack elides each label to its row.
Neither is cropped at `FIGURE_MAX_ROWS`, since there is no full screen to
show the rest, and their own caps (`MAX_EVENTS`, `MAX_FRAMES`) bound them.
A mermaid `subgraph` tags its members with its outermost id, and a node
first named on an edge joins the first subgraph that lists it. Once laid
out, the engine outlines a subgraph only when its members land contiguous,
no foreign node inside their margin and no other outline overlapping; the
layout reserves that margin only when the model carries a subgraph, so an
ordinary flowchart pays nothing for it. Outlines draw before the edges, so
a crossing edge merges into a junction and keeps its arrowhead, and the
title goes on whichever border has room clear of an arrowhead. A
`{decision}` node draws with a `◇` marker and earns none of the "drawn as a
box" notes the other shapes the engine cannot render get. What was
simplified comes back in the tool's reply, so the agent learns and the
reader never sees a gap; only a mermaid diagram with
no node-and-edge shape at all (and no `sequenceDiagram` header) fails, and
its source stays in the body as prose. Parsed bodies are cached on
`DiffView` keyed by comment id, or
by `summary_figure_key(&walkthrough.id)` for the summary's own body, and a
hash over the body and the wrap width, so a diagram is parsed when it
changes and not per frame; the same key scheme carries into the per-frame
figure rasteriser, so a summary's figure draws through the exact path a
stop's does. `o` opens a `Graph` figure full screen; a `Text` figure has no
full screen to open, so `o` on one says as much and names `<cr>`: on a
figure row whose drawing names a resolved node (a callstack frame's own
anchor, or either row of a message to a participant that mermaid's own
`link <participant>: <label> @ <target>` statement anchors), `<cr>` in the
diff pane jumps straight to that code, through the same `resolved` map a
graph's `click` targets fill.
A body is capped at 8KB and a walkthrough at 64KB, since parsing and layout
run on the thread serving the TUI and the text comes from an agent; the
summary counts toward the same two caps, `BodyTooLong` and `TotalTooLong`,
as any stop's body. `MAX_STOPS` (20) is a rail against dumping the diff,
never a target, and the skill asks for one stop per real decision, plus one
summary naming the shape of the whole change rather than listing them.
Resolution reads files, so it is a worker like any other
(`pending_walkthrough` → `AppEvent::WalkthroughAnchors`, token-guarded) and
the slide on screen seats nothing until it lands, at which point the rows
rebuild and the slide reseats, its stop through `seat_stop`, its ad hoc
comment through `focus_comment`, or the summary through `seat_summary`, so
its region appears under the reader; a figure node whose symbol is gone
renders stale and its header counts them. The summary's own figure targets
resolve the same read-once pass, since it is one more entry in the same
figure cache the worker's file list is built from.
A stop or note anchored outside the diff still gets a slide: the anchor
worker's read becomes a `FileStatus::Unchanged` `FileDiff` (one hunk, every
line `Context`, `old_text` and `new_text` both the file's own content) on
`DiffView::context_files`, appended after the diff's own files by
`DiffView::model_with_context` wherever the walkthrough layout reads the
model (`ensure_rows`, `seat_stop`, the pane's own render), a path already
in the diff never duplicated; every other layout sees the diff alone.
The per-frame enrichment queues them too, so they highlight like any file.
A walkthrough is pinned to the commit checked out when it is published:
`agent_publish_walkthrough` stamps `Walkthrough.rev` with the full oid of
`HEAD` on every call, a revision included, since each one redescribes the
stops against whatever is checked out at that moment. The anchor worker
reads each file through `Review::compute_walkthrough_files`, from the copy
`ReadFirst` names and else the other: `rev` (`Vcs::read_at`) first for a
walkthrough about a commit, a range or a PR, and the worktree first for one
about the working tree, since that code was never in the pinned commit and
`rev` only serves a file deleted since. A walkthrough saved before `rev`
existed reads the worktree alone. `WalkthroughRequest` carries `read_rev`
and `read_first` alongside the files to read. A file the diff itself
carries skips that read when its anchors resolve: they resolve against the
diff's new side, the text the slide shows.
A reader marks a slide read with `m` in the walkthrough layout
(`Session::seen_stops`, pruned to the open source's own `stops`
on every change); it advances to the next slide the way `m` on a file
advances to the row below, and `u` jumps to the next unseen slide, wrapping,
saying so once every slide is; the summary carries no seen mark of its own,
so `m` and `u` pass over it. The diff sidebar's own heading, which counts
the walkthrough's stops, shows
`seen/total` once at least one is seen, else just the total, and a seen
stop's sidebar row carries the same dim `✓` a viewed file row does; the
status screen's row for a walkthrough carries the same `✓` once every one
of its stops is seen. `d` on
the walkthrough layout's leading `Summary` row asks before deleting the
walkthrough's review file and every comment in it; `d` on a stop row asks
before deleting just that stop and its notes (found by anchor containment:
every comment inside the stop's own region), through the same
`Modal::Confirm` + `PendingOp` shape a file discard uses; deleting the
whole walkthrough (`store::delete_source`) closes the diff first when it
is the one open, and forgets the source's cached session
(`Review::forget_source`) so a later access reloads nothing stale. A
walkthrough with no summary has no leading row, so the sidebar carries no
way to delete the whole thing; the status screen's own `d` on
a walkthrough row asks the same before deleting that one by id regardless
(the header takes no special action, like every other header; everywhere
else on that screen `d` is still the diff transient). The pane's own
`d` on a card keeps deleting just that one comment. Committing from inside
diffler carries nothing: a walkthrough is its own source, with no tie to
the working tree or to the commit a `c c` makes from it.
