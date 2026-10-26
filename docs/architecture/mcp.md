# MCP server

Tools: `review_status`, `get_diff`,
`get_comments`, `list_reviews`, `reply_comment`, `propose_resolve`,
`mark_viewed`, `add_comment`, `delete_comment`, `edit_comment`,
`report_activity`, `wait_for_feedback`, `publish_walkthrough`,
`get_walkthrough`, `get_pr`, `open_project`, `focus`. `add_comment`
writes a new comment on a line or an inclusive line range of the review the
human is currently looking at, anchored exactly the way a human's own
comment is; authored as the agent by default, or as the human when
`as_human` is set, so it goes out untouched with their next submitted
review. A line outside the diff is refused unless `outside_diff` is set,
because the human reads the diff; with it, the comment anchors to the file's
own line (the working-tree review only) and waits in the human's
comment list. `delete_comment` and
`edit_comment` take back or rewrite any local comment, the human's
included (so the agent can redraft what the human asked it to), keeping
its author, so a comment that has drifted or turned out wrong need not sit
answered by another comment stacked on top; both refuse one already on the
forge (`comment.remote_id` set), since the next sync puts the forge's text
back, and a walkthrough stop or note (`comment.anchor_ref` set), since
those two live and die with
`publish_walkthrough`, which already tracks their ids and threads across a
revision. `delete_comment` also refuses one someone else has replied to,
since a reply lives inside its comment and would otherwise disappear with
it unseen; `edit_comment` never touches replies, so it carries no such
refusal.
`focus` (`app/diff/focus.rs`) moves the human's view: to a comment or a
walkthrough by id, found in whichever review source holds it, or to a file
and line range in a named review (`working`, `commit:<rev>`,
`range:<oldest>..<newest>`, `pr:<n>`, `against:<rev>`, or a stored source
key), else the review on screen when it holds the file, else the working
tree, else the file view. It runs the same opens and seats the keys do
(`open_commit_diff`, `open_pr_review_for`, `open_walkthrough`,
`focus_comment`, `reveal_line`), popping the stack back to its root first
so repeated jumps never pile screens up, and a line no hunk shows opens the
file view there. The workspace routes an id to the tab owning it and brings
that tab to the front. It refuses while a composer or a dialog is open, in
the tab in front or the target one, since a jump would hide the draft, and
while the open view holds a parked draft it would have to replace, since
popping that view drops the draft. A PR the list has not loaded or whose
head is not fetched answers `waiting` and parks the focus in
`App::pending_focus`, the one slot it waits in (it takes back the
`pending_walkthrough_open` a walkthrough about a PR sets); the PR list and
the head fetch retry it, and a failed list drops it.
`report_activity` names the agent's own focus (and the file it's about, if
any) for the status bar's live indicator; every other tool call already
counts as activity on its own, mapped to a plain phrase (`app/mcp.rs`'s
`record_mcp_activity`) so a call that carries no report never reads as
idle, and `wait_for_feedback` sends `AppEvent::McpWaiting` with its poll's
deadline before it blocks, since the poll sends no request until the human
answers. The indicator drops `AGENT_ACTIVITY_TTL_TICKS` (45s) after the last
call, or after a poll's deadline, which can outlast that ttl; it lives in
memory only, shows whichever MCP session reported most recently, and takes
only the status bar's leftover width, so the viewed count and a message
always win.
Comments are tagged with their source. Agent triggering is the
`wait_for_feedback` long-poll (MCP can't initiate agent turns); the human's
"send" key unblocks it. `propose_resolve` only marks a comment Replied, and
writes nothing into the thread: the prompt has agents reply then flag, so a
note there would restate the answer. Its note lands only when the agent has
not replied to that comment. Only the human resolves it, in the TUI.
A walkthrough stop is a comment in its own source's session, so a remark
on one arrives through `wait_for_feedback` (which reads across every
source) as a reply on that comment, tagged with a `source` of
`walkthrough-<id>`, and its comment id names the stop; a stop's `notes`
are extra remarks on other parts of its own region, each its own titleless
comment. `publish_walkthrough` takes an optional
top-level `id`: passed, it revises that walkthrough's own source in place;
omitted, it creates a fresh source with a random id. It also
takes an optional `id` per stop and per note to keep that comment and its
thread across a revision, and its response reports the `rev` the
walkthrough is now pinned to. `get_walkthrough` takes an optional `id` (the
newest walkthrough on disk when omitted) and returns every id, notes and
`rev` included, to pass back. `review_status` carries a `walkthroughs` list (id,
title, stop count, publish time), read from every walkthrough source on
disk, newest first, empty when none, so a fresh
agent knows which ones exist, and whether one already covers this change,
before it reads anything else. The TUI also
registers itself under a per-user registry (`$XDG_STATE_HOME/diffler/instances`),
so the `diffler-mcp` stdio proxy's own `list_instances`/`use_instance` tools
can find and target a diffler running in a different repo than the agent's
shell.
