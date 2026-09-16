# MCP tools

While the TUI is running, diffler serves an MCP server at `127.0.0.1:{port}/mcp`
(the live port is published to `.diffler/mcp.json`). An agent connects through it
to read your review and respond. Without a daemon, the tools are only available
while diffler is open.

## Read

- **review_status**: current review: repo, branch, changed files with their viewed marks, comment counts, the feedback epoch, and every walkthrough published in the repo (id, title, stop count, publish time), newest first, empty when none has been published. `corrupt_reviews` names any review file that failed to parse and was skipped, so a missing review or walkthrough reads as that rather than a clean repository.
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
- **reply_comment**: answer a comment in place; you see the reply immediately.
- **propose_resolve**: mark a comment replied. Adds nothing to the thread, so an answered comment carries the answer alone; the note lands only when the agent has not replied to that comment. Only you resolve it, in the TUI.
- **mark_viewed**: mark a file viewed in the review you're currently looking at.
- **wait_for_feedback**: long-poll until you send feedback (a comment, reply, or the send key), then return the new epoch and all open/replied comments. A comment on a walkthrough stop is a reply on that stop's own comment, so its id names the stop. This is how the agent waits for its turn. It answers within 55 seconds; the agent polls again to wait longer.
- **publish_walkthrough**: publish the agent's reading order for a change, one stop per real decision and as few as the change needs, as its own review, with its own comments and viewed and seen marks. Pass `id` (from `review_status` or `get_walkthrough`) to revise that walkthrough in place; omit it to publish a new one of its own. Every stop becomes an agent comment you reply to in place; a stop's `notes` are its own extra remarks on other parts of the same region, each its own comment. `summary` is the walkthrough's own overview: one short paragraph plus one `mermaid` flowchart of the shape of the whole change, never a list of the stops; capped like a stop body and counted toward the walkthrough's total cap. On a revision, a stop or note that passes its `id` back keeps its comment and the thread on it; one whose id is left out is deleted. Every publish, a revision included, pins the walkthrough to the commit checked out at that moment, so its stops resolve against the code they describe even after the branch moves on. The reply carries the walkthrough's `id`, that `rev`, and validation receipts; a refusal names what to fix and republish.

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
