---
description: Check the diffler review and respond to the human's feedback
---
Check the diffler review and respond to the human's feedback:

1. Call the diffler MCP tool `review_status` for the active review and its
   changed files. If `walkthroughs` holds none about a change you made, call
   `publish_walkthrough` first (see /dfa), then continue. `review_status`
   names the `repo`; if it is not the repository you mean, call
   `list_instances`, then `use_instance` with that repo, and start over.
2. Call `get_comments` with status "open" and read each comment in place.
   Each carries a `source`: the human's review comments on the diff itself
   carry the diff's own source, while one starting `walkthrough-` is feedback
   on that walkthrough, a reply on one of its stops or notes.
3. Address every comment in the code it anchors to.
4. Answer each with `reply_comment` (what you changed and why), then
   `propose_resolve` to flag it addressed. That flag adds no text: do not
   summarise the answer you just gave. Only the human resolves for real, in
   the TUI.
5. Call `wait_for_feedback` with the latest epoch and start over when it
   returns: the human just sent new feedback. It answers within 55 seconds; a
   `timed_out` result means they are still reviewing, so call it again. If the
   call itself fails, check `review_status`: diffler is closed only when that
   fails too, otherwise keep waiting.

## Write every reply like this

- Say what you changed and why, in two to four short sentences or bullets.
  Lead with the change, then the reason.
- Answer the question that was asked. If you disagree, say so in one sentence
  and say why.

## Write for the card

Write for a narrow card beside the code:

- Full sentences with their articles and pronouns, in the first person plural
  when we are the subject: "We changed X because Y." A sentence may also lead
  with the thing itself: "`infra` runs first because Y." Do not open every
  sentence the same way. Cutting words does not make text clearer.
- Put every identifier in backticks: `compute_overrides`, `SHEET_PERMISSIONS`,
  `MissingDecision`. The card renders markdown; bare identifiers read as prose.
- Plain verbs for code: raise, return, check, read, write, fail, skip. No
  metaphor: no gate, carry, land, surface, shape, vote, mint, unpick, wall,
  hold an opinion.
- Comparing things, or listing data with several fields per item: a markdown
  table, header row, two to four columns, one row per thing, identifiers in
  backticks inside cells. The card rules the columns.

If the diffler tools are missing, the TUI isn't running: ask the human to
run `diffler` in the repository first.
