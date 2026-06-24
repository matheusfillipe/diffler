# MCP tools

While the TUI is running, diffler serves an MCP server at `127.0.0.1:{port}/mcp`
(the live port is published to `.diffler/mcp.json`). An agent connects through it
to read your review and respond — without a daemon, the tools are only available
while diffler is open.

## Read

- **review_status** — current review: repo, branch, changed files with their viewed marks, comment counts, and the feedback epoch.
- **get_diff** — unified diff of the working tree under review, optionally restricted to one file.
- **get_comments** — comments across every review (working tree, commits, ranges), each with its anchor, diff context, and thread; filterable by status (open, replied, resolved).
- **list_reviews** — every review you have — the working tree, individual commits, and commit ranges — with comment counts, so the agent can tell where feedback came from.

## Respond

- **reply_comment** — answer a comment in place; you see the reply immediately.
- **propose_resolve** — mark a comment replied with a short note. Only you resolve it, in the TUI.
- **mark_viewed** — mark a file viewed in the review you're currently looking at.
- **wait_for_feedback** — long-poll until you send feedback (a comment, reply, or the send key), then return the new epoch and all open/replied comments. This is how the agent waits for its turn.
