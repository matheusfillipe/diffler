---
name: dfr
description: Review a change in diffler and leave comments on real problems, never submit. Use when the user invokes /dfr, asks the agent to review a pull request or change in diffler, or wants review comments left for them instead of an explanation of what changed.
---

Review the active review in diffler:

1. Call `review_status`. It names the `repo`; if it is not the repository
   you mean, call `list_instances`, then `use_instance` with that repo, and
   start over.
2. Read the diff with `get_diff`.
3. Read it as a reviewer, not an explainer. Look for:
   - **Correctness**: logic errors, the wrong branch or operator, edge cases
     (empty input, `None`/`null`, negative numbers, concurrent callers),
     security holes (injection, a missing auth check, a secret in a log).
   - **Reuse**: a helper the repo already has, reinvented; an abstraction
     with one caller; a file the stated change does not need.
   Skip anything a linter or formatter would already catch, and any nitpick
   you cannot back with a concrete trigger.
4. For each real problem, call `add_comment` on the exact line or range it
   is about (`line_end` only past one line), a body written as the section
   below says. One comment per problem, none for what is fine.
5. A comment of yours drifts or turns out wrong (`get_comments` marks it
   `outdated`, or the branch moved on): fix it with `edit_comment`, or
   retract it with `delete_comment`. Never stack a new comment on a stale one.
6. You never submit the review. Only the human presses the submit key,
   after reading what you wrote, so say this plainly when you report back.
7. Write as yourself by default, so the human answers you in the thread.
   Pass `as_human` on a comment only when they ask for a draft they will
   send as their own.
8. Tell the human the comments are ready; run /df to keep answering their
   feedback on them afterward.

## Write every comment like this

- Say what is wrong and why, in one or two short sentences. Lead with the
  problem, not a description of the line.
- Name the concrete trigger: the input or path that makes it fail.

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
