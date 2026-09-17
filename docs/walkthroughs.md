# Walkthroughs

A walkthrough is an agent's reading order for a change: a summary, then one
stop per decision, each anchored to the code it is about. The agent that wrote
the change is the only party who knows which parts matter and in what order,
so it says so and you read along.

Ask for one in your agent:

```
/dfa
```

## Reading one

The status screen lists every walkthrough the repository has under
`Walkthroughs`, newest first, with its stop count. `<cr>` opens one.

A walkthrough opens on its **summary**: a paragraph on what the change does
and a diagram of its shape. After that the sidebar lists the stops. Each stop
is a slide showing its own region of code, the agent's card, and any comment
anchored inside it.

| Key | Action |
| --- | --- |
| `<cr>` | open the walkthrough under the cursor, or one of its stops |
| `j` / `k` | walk the stops in the sidebar |
| `]` / `[` | step to the next or previous comment, entering its slide |
| `m` | mark this slide seen and move to the next |
| `u` | jump to the next unseen slide |
| `r` | reply to the agent on the card under the cursor |
| `c` | comment on the code the stop points at |
| `e` | open that code in `$EDITOR` |
| `o` | open the figure under the cursor as a graph you walk node by node |
| `d` | delete the stop under the cursor, or the whole walkthrough from the status screen |
| `t` | cycle back to the file tree and the other sidebar layouts |

Reply on a stop and the agent picks it up the same way it picks up any review
comment: answer in its thread, or ask it to rewrite the walkthrough.

## Where it lives

A walkthrough is a review of its own, stored in
`.diffler/reviews/walkthrough-<id>.json` beside the working-tree, commit,
range and pull-request reviews. Its comments and marks stay in it, so deleting
every comment in the working-tree review leaves it untouched and nothing it
holds is ever posted to a forge.

It is pinned to the commit it was published against, so it still reads
correctly after you switch branches: each stop's code is shown as it stood.
A stop whose file or symbol has since gone says so on its card.

## Diagrams

A stop's body renders markdown, and a ```mermaid fence becomes a figure drawn
in the terminal. Only the `flowchart` subset is drawn; anything else is
simplified, and the agent is told what was simplified so it can adjust. A
figure too wide for the card is redrawn top to bottom, and `o` opens any
figure full screen, where `<cr>` on a node jumps to the code it names.

## Writing one, as an agent

The `/dfa` skill carries the full instructions. In short: publish through
`publish_walkthrough`, one stop per real decision, as few as the change needs,
each with a title, an anchor (`path#symbol`, `path:start-end`, or `path`) and a
body. Pass a walkthrough's `id` back to revise it in place and keep the threads
hanging off its stops. `get_walkthrough` returns every id to pass back, and
`review_status` lists what the repository already has.
