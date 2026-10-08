# MCP tools

While the TUI is running, diffler serves an MCP server at `127.0.0.1:{port}/mcp`
(the live port is published to `.diffler/mcp.json`). An agent connects through it
to read your review and respond. There is no background service, so the tools
only work while diffler is open.

## Read

- **review_status**: the review you have open: repo, branch, changed files with their viewed marks, comment counts, the feedback counter `wait_for_feedback` takes, and every walkthrough published in the repo (id, title, stop count, publish time), newest first. `corrupt_reviews` lists any review file that could not be read and was skipped, so the agent knows a review is missing, not empty.
- **get_diff**: unified diff of the working tree under review, optionally restricted to one file.
- **get_comments**: comments across every review (working tree, commits, ranges, PRs, walkthroughs), each with its anchor, diff context, thread, and source; filterable by status (open, replied, resolved). A comment whose source starts with `walkthrough-` is feedback on that walkthrough.
- **list_reviews**: every review you have (the working tree, individual commits, commit ranges, and walkthroughs) with comment counts, so the agent can tell where feedback came from.
- **get_walkthrough**: the walkthrough its `id` names (from `review_status`), or the newest one when `id` is omitted, or null when none has been published. A walkthrough carries its title, author, stops, what was skipped, its `summary`, when it has one, and the full commit `rev` it is pinned to, `null` for a walkthrough published before `rev` existed. Each stop carries the id of the comment it is and its `notes`, extra remarks on other parts of its own region, each with its own id; those ids are what a revision passes back.

## Respond

- **add_comment**: write a new comment on a line or an inclusive line range
  of a file, in the review you're currently looking at. Anchored exactly the
  way a human's own comment is, so a rewrite marks it outdated the same way;
  a range's start and end both have to land in the diff, and in the same
  hunk. The body is trimmed and has to say something, capped like a stop's.
  Authored as the agent by default, so the human answers it in the thread;
  pass `as_human` to author it as the human's own instead, so it goes out
  untouched with their next submitted review.
- **delete_comment**: delete a comment you wrote with `add_comment`. Refused
  for a human's own comment, for a walkthrough stop or note (revise or
  drop those with `publish_walkthrough` instead, which already tracks their
  ids and threads), and for one someone else has replied to (a reply lives
  inside its comment, so deleting it would take the reply down too; edit the
  body instead).
- **edit_comment**: replace the body of a comment you wrote with
  `add_comment`, keeping its status, replies, and anchor. Same refusals as
  `delete_comment` except the reply one, since editing never touches replies.
- **reply_comment**: answer a comment in place; you see the reply immediately.
- **propose_resolve**: tell you a comment is dealt with, by marking it replied. Its optional note goes into the thread only when the agent has not replied there yet, so an answered comment keeps just the answer. Only you resolve it, in the TUI.
- **mark_viewed**: mark a file viewed in the review you're currently looking at.
- **report_activity**: say what the agent is doing right now, in a few words and optionally the file, in your status bar. Every other tool call already shows there on its own; the indicator clears 45 seconds after the last call.
- **wait_for_feedback**: wait until you send feedback (a comment, a reply, or the send key), then return a new feedback counter and every open or replied comment. A comment on a walkthrough stop is a reply on that stop's own comment, so its id names the stop. This is how the agent waits for its turn. It answers within 55 seconds; the agent polls again to wait longer.
- **publish_walkthrough**: publish the agent's reading order for a change: one stop per real decision, as few as the change needs. It becomes a review of its own, with its own comments and viewed and seen marks.
  - **What it describes:** whichever review you have open right now (the working tree, or a commit, range or PR diff), as `review_status` reports it. Revising a walkthrough while looking at its own diff keeps what it already described.
  - **New or revised:** pass `id` (from `review_status` or `get_walkthrough`) to revise that walkthrough in place; leave it out to publish a new one. On a revision, a stop or note that passes its own `id` back keeps its comment and thread; one left out is deleted.
  - **Stops and notes:** every stop becomes an agent comment you reply to in place. A stop's `notes` are extra remarks on other parts of the same region, each its own comment, anchored in the stop's own file.
  - **Anchors:** a stop's `anchor` may be left out to hang it on the walkthrough's file, taken from another stop's anchor or the open review's first file. Every anchor has to name a file the review can reach: one in its diff, or one still readable on disk.
  - **Summary:** `summary` is one short paragraph plus one diagram of the whole change's shape (a `mermaid` flowchart, a `mermaid` `sequenceDiagram`, or a `callstack` tree), never a list of the stops. It is capped like a stop's text and counts toward the walkthrough's total cap.
  - **Pinned:** every publish, a revision included, pins the walkthrough to the commit checked out at that moment, so its stops still show the code they describe after the branch moves on.
  - **Reply:** the walkthrough's `id`, the pinned `rev`, and what was checked. A refusal (an anchor naming nothing real, or nothing to fall back on) says what to fix, and nothing is stored until it is fixed.

## Prompt

- **review**: the check-and-respond loop as a client command (`/diffler:review`
  in Claude Code): read open comments, address them, reply, wait for the next
  round.
- **walkthrough**: publish a walkthrough of a change and answer the human's
  comments on it (`/diffler:walkthrough`), the same steps as the `dfa` skill.
- **critique**: review a change and leave comments on real problems
  (`/diffler:critique`), the same steps as the `dfr` skill. It never submits
  the review; only the human does that.

## Cross-repo discovery (proxy only)

These two tools live in the `diffler-mcp` stdio proxy, not the TUI: a human's
diffler and an agent's shell are often in different repos, so the proxy keeps
a per-user registry of every running instance and can retarget itself.

- **list_instances**: every running diffler this proxy can reach, across all
  repos.
- **use_instance**: point this proxy at one of them, by repo path (or a
  unique directory-name suffix) or port, for the rest of this session.
