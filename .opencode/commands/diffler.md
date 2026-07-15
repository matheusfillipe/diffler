---
description: Check the diffler review and respond to the human's feedback
---
Check the diffler review and respond to the human's feedback:

1. Call the diffler MCP tool `review_status` for the active review and its
   changed files.
2. Call `get_comments` with status "open" and read each comment in place.
3. Address every comment in the code it anchors to.
4. Answer each with `reply_comment` (what you changed and why), then
   `propose_resolve`; only the human can resolve for real, in the TUI.
5. Call `wait_for_feedback` with the latest epoch and start over when it
   returns. If it times out, call it again; if the connection fails, diffler
   is closed, so stop.

If the diffler tools are missing, the TUI isn't running: ask the human to
run `diffler` in the repository first.
