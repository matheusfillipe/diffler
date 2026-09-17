---
name: dfa
description: Walk the human through a change in diffler, publish a walkthrough with one stop per real decision and answer their comments. Use when the user invokes /dfa, says "walk me through", or asks the agent to present or explain what it changed.
---

Walk the human through `$ARGUMENTS`, or through everything you changed in
this review when `$ARGUMENTS` is empty:

1. Call `review_status`. It lists the repo's `walkthroughs`. When one of
   them is about the change you are presenting, call `get_walkthrough` with
   its `id` and revise it, passing that `id` to `publish_walkthrough`; when
   the change is a different one, publish a new walkthrough without an `id`.
   `review_status` names the `repo`; if it is not the repository you mean,
   call `list_instances`, then `use_instance` with that repo, and start over.
   A walkthrough is pinned to the commit it is published at, so publish it
   once the code it describes is the code checked out; once the branch has
   moved on, revise it so the stops point at the revision it is on now.
2. Read the diff with `get_diff`.
3. Choose one stop per real decision, as few as the change needs: five is
   common, ten is a lot, and more means the change wants splitting. Order
   them as a reader should meet them. Each stop is a span (`path#symbol`
   where a symbol exists, `path:start-end` otherwise), a title, and a body
   written as the section below says. A stop's span is the lines a marker
   would highlight, usually three to fifteen; a whole function only when the
   whole function is the decision. A bare `path` only when the file itself is.
   A stop is a topic, not just a span: use `notes` for a second remark on
   another part of the same region, or a diagram beside the prose, each
   anchored in the stop's own file (`path:line` or `path:start-end`, or
   omitted to sit at the region's first line).
   Say each decision once. No overview stop that lists what the stops after
   it will say, and no closing stop that repeats them: the title is the
   overview, and a map of the whole change belongs in the summary below, not
   a stop.
4. Add a `mermaid` flowchart whenever a stop describes a flow, a sequence of
   calls, or a branch with more than two outcomes. A shape beats a paragraph.
   Several fences in one body are fine, and a note can carry one of its own
   when a different part of the region needs its own diagram.
5. Put what you left out in `skipped`.
6. Write a `summary`: what the reader meets first. One short paragraph saying
   what the change does, plus one `mermaid` flowchart of the simplest shape
   that explains it, five to eight nodes, naming real files or functions. It
   never lists the stops and never repeats their titles: the stops are the
   detail, the summary is the shape.
7. Call `publish_walkthrough`, read its receipts, fix a refusal and
   republish. When you revise, pass each surviving stop's and note's `id`
   from `get_walkthrough` so it keeps its comment and the thread hanging off
   it; one whose id you leave out is deleted.
8. Tell the human the walkthrough is on the status screen under Walkthroughs.
9. Call `wait_for_feedback` in a loop: it carries feedback on every review, so
   check each comment's `source`. One starting `walkthrough-` is a reply on a
   stop or note of that walkthrough, arriving as a reply on that comment; the
   id names which one. Answer in its thread with `reply_comment`. Revise the
   walkthrough with `publish_walkthrough` when asked to change it, and keep
   waiting.

## Write every stop like this

The human reads the title in a sidebar about thirty characters wide and the
body in a narrow card while looking at the code.

- The title is a label, two to five words, no verb and no "We": "Two
  terraform roots", "Postgres node pool", "Bootstrap hand-off to ArgoCD". It
  names the thing the stop is about; the body says what we did with it.
- The body is two or three bullets, each one short sentence. Say what we chose
  and why; add what would go wrong otherwise only when it is not obvious.
- One idea per bullet, at most one line of about eighty characters. Vary how
  a bullet opens; "We" is fine when it is the natural subject, and a bullet
  can also start with the thing itself: "`infra` runs first because ...".

Example:

```
Missing owner list
- `MissingDecision` is raised when `OWNER_EMAILS` is empty.
- A default would silently migrate the rows an owner still has to claim.
- The run stops and names what is missing, at build time, never at login.
```

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
