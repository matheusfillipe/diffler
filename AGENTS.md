# diffler: agent guide

Terminal code-review companion for AI agents. Launched in a repo, it renders a
live neogit-style git UI and embeds an MCP server, so an agent reads review
comments in place, replies, and reacts to feedback. The human reviews and drives
git; the agent responds; the diff updates live. Philosophy: YAGNI/KISS (one
small native binary, alternate-screen TUI, no daemon, no browser).

## Layout

```
crates/diffler-core/   pure logic, no terminal (errors via thiserror):
  vcs.rs / git.rs      Vcs trait + git2 backend (status, diff, log, stage, commit, branch)
  repo.rs              repository discovery (finds the repo root from any path)
  model.rs diff.rs     diff model, hunks
  pairing.rs           similarity line-pairing + grapheme intraline emphasis
  syntax/              tree-sitter language registry + AST-diff intraline emphasis + scope index
  highlight.rs         syntect whole-file highlight
  source.rs review.rs  ReviewSource + per-source review state
  session.rs           comments (a walkthrough stop is one) + viewed marks
  walkthrough.rs       the agent's reading order: the stops' comment ids, anchors
  store.rs             .diffler/ persistence
  feedback.rs          markdown feedback export

crates/diffler/        binary (color-eyre at the top; thiserror for typed errors):
  ui/ app/ tree.rs     ratatui TUI: screens, file sidebar, state
  app/composer.rs      in-place comment editor (app/text_edit.rs is its key set)
  ci/                  forge seam: CI acquisition + PR review (ForgeProvider trait; gh/glab/Forgejo REST)
  graph/               navigable orthogonal node-graph ratatui component
                       (mermaid.rs parses the flowchart subset an agent writes)
  keymap.rs config.rs  configurable keybindings, layered TOML config
  theme.rs transient.rs  rendering theme, popup/modal model
  mcp.rs               rmcp/axum MCP server
  watch.rs             notify filesystem watcher
  editor.rs clipboard.rs  $EDITOR suspend/restore, OSC52 yank
```

## Commands (just; see `just --list`)

- `just check`: clippy with ci's denials, run after every change
- `just test`: nextest + doctests
- `just fix`: clippy --fix + fmt
- `just snap`: insta snapshot tests; read `.snap.new` diffs before `just snap-accept`
- `just e2e`: PTY end-to-end suite (needs `uv`; CI runs it in a separate job)
- `just package-check`: what crates.io builds. A crate packages only its own
  directory, so a file it reaches outside one builds here and fails the publish
  after the tag is public. `just ci` carries the include rule; the release
  script runs the whole check.
- `just ci`: fmt+clippy+tests gate, must pass before any commit (CI additionally runs msrv, deny, typos, dupes, machete, coverage)
- `showcase/record.sh`: regenerate `showcase/img/*.png`, one screenshot per theme
  (needs `vhs`). It seeds a throwaway repo with a three-file review and shoots
  the review screen with all three panes up, the comments sidebar included, so
  rerun it after anything that changes how that screen looks. `record.sh --seed`
  prints the seeded repo and records nothing, for checking the frame first.
  The README's hero `assets/demo.gif` is hand-recorded and has no script.

## Rules

- Code is done only when `just ci` passes. Run it, don't assume.
- No `unwrap`/`panic!`/`todo!` in non-test code (clippy denies). `expect` needs justification.
- Clippy scores every function's cognitive complexity and warns over 20, which
  is a warning CI denies. The two dispatch loops above it carry an allow and a
  reason; a third means the function grew a second job, so split it.
- Errors: `thiserror` for typed library-style errors (diffler-core, the `ci` module); `color-eyre` for the binary's top level only.
- No `println!`/stdout writes in the TUI (corrupts the screen; clippy denies it).
- Async: never block in async fns; `spawn_blocking` for CPU/IO-heavy work.
- TUI changes need TestBackend + insta snapshot coverage. A changed snapshot is a
  behavior change: read the diff, never accept blindly, never edit `.snap` by hand.
- Run `just e2e` after rendering/behavior changes: `just ci` skips it, and glyph
  or timing changes can pass ci yet break the PTY suite.
- PTY e2e probes must drain output continuously (the suite's wait helpers do);
  a bare sleep fills the PTY buffer and freezes the app under test.
- Test fixtures and sample data use generic mock names ("reviewer",
  "acme/widgets"), never real usernames, handles, or emails.
- Hooks are managed by prek (`prek install` once). If a hook fails, fix the cause.
  Never `git commit --no-verify`.
- Review before committing: in Claude Code run `/rev` on the working tree for any
  non-trivial change.
- Commit messages: short, imperative, one line. No body unless the why is non-obvious.
- Comments explain why, never what. No change-history commentary.
- UI text is imperative and names the action: a hint is `c add comment`, never
  `c comment` (bare noun) or `c writes the first` (narrating what the program
  does). An empty state names the key that fills it. Applies to hints, messages,
  buttons and confirmations, never to code comments or docs.
- New dependencies: add to `[workspace.dependencies]`, justify in the commit.

## Architecture & decisions

- **Layering.** Nothing above the `Vcs` trait may import git2. Only the git2
  backend exists; the trait is there because jj is planned, but no second
  backend is built or stubbed (YAGNI).
- **Runtime.** One tokio runtime: MCP server (axum, `127.0.0.1:{port}/mcp`),
  notify watcher (debounce ~200ms → refresh), main task = the ratatui loop.
  `App` owns all state; workers (git, CI, editor, clipboard, refresh,
  enrichment) are spawned off "pending" slots and answer over the event
  channel. Watcher refreshes and per-file enrichment (emphasis/highlight/
  scope) run on the blocking pool: the pane renders plain until results
  land; draw never computes. Caches: hash-memoized per-file hashes, enriched
  models, commit/range models, CI workflow YAML. Perf guard: `just bench`
  (criterion, recorded on main by CI) + `tests/e2e/test_perf.py` ceilings.
- **Review state is per diff source.** A `ReviewSource` is `WorkingTree`,
  `Commit{oid}`, `Range{oldest,newest}`, `Pr{number}`, `Against{rev}`, or
  `Walkthrough{id}`. Comments (anchored to file + line +
  a `line_text` snapshot so stale anchors show as outdated; visual mode anchors a
  range; status Open/Replied/Resolved + threads) and GitHub-style viewed marks
  (keyed by file content hash, auto-cleared on change) are stored **per source**:
  `.diffler/reviews/<key>.json` where key ∈ {`working`, `commit-<oid>`,
  `range-<a>-<b>`, `pr-<n>`, `against-<rev>`, `walkthrough-<id>`}. Legacy
  `.diffler/session.json` migrates to `reviews/working.json`; a review file
  written before a walkthrough was a source of its own, carrying it embedded,
  splits it into its own `walkthrough-<id>.json` on load (see Walkthrough,
  below).
  `.diffler/` self-gitignores. No daemon: agent tool calls fail while the TUI is
  down (by design, harnesses retry).
- **Three-dot review (`Against{rev}`).** `d` on the status screen opens the diff
  transient: the base branch, `HEAD~1`, a branch, a commit from the log, or back
  to the plain working tree. The diff is `merge-base(rev, HEAD)` vs
  index + worktree + untracked, so the whole branch reads as one review,
  uncommitted work included, with no PR. `rev` is stored as the human named it
  and resolved at diff time, so the review follows the ref. The model is live,
  not pinned like a commit's: `App::against_rev` rides along with every queued
  refresh and `Review::compute_refresh` rebuilds it on the blocking pool, then
  `apply_refresh` swaps it into the open view (fingerprint-guarded, cursor and
  folds kept). Keys collapse `/` to `-`, so `feat/x` and `feat-x` share a
  review file.
- **Diff pipeline.** git2 hunks → similarity line-pairing → grapheme intraline
  emphasis → syntect whole-file highlight sliced onto diff lines → composite
  (syntax-fg over diff-bg over emphasis-bg). GitHub-dark default theme;
  progressive render (a plain first frame is fine).
- **Grammars.** `syntax::registry::REGISTRY` is one process-wide `LazyLock`
  holding every bundled grammar; a language compiles its highlight query on
  first use (~15ms) behind a `OnceLock`, on the enrichment thread. Registering
  a grammar is therefore free until someone opens that language, and a theme
  switch reuses the compiled queries. Some grammars extend another (`cpp` over
  `c`, `svelte` over `html`, `tsx` over `js`+`ts`): register the concatenation
  or the query silently matches almost nothing. `every_language_colours_a_sample`
  in `highlight.rs` is the guard.
- **TUI.** neogit/doom keybindings, every binding configurable. Screens: Status
  (the branch band, a rule, then the repo band; stage/unstage/
  discard/commit/branch), Log, Diff/review (file sidebar + pane, unified or
  `|`-toggled side-by-side; `c` comment, `V` visual select, `r` reply/resolve,
  `m` viewed, `y`/`Y` yank feedback as markdown, `e` `$EDITOR` jump). Every motion, the
  paging keys included, moves the pane holding the keyboard: `<c-d>` walks the
  file list from the sidebar, the cards from the comments pane (counted in
  cards, since they are multi-line), and the rows from the diff. `C` opens
  the comments sidebar on the right, a third pane the motions walk: its
  selection seats the diff cursor on that comment, so the pane's own verbs
  (reply, resolve, delete) reach it with no handling of their own. Comments,
  replies and edits are written in place: the composer occupies the rows the
  finished card will, under the anchored line, at the top of the file for a
  whole-file comment, under the thread for a reply. Runs (the
  CI run list), Graph (CI run detail on the shared node-graph component), Prs
  (open PRs of the repo's forge), CiLog (a
  job's log folded into its real steps), and File (below). The diff sidebar has three
  layouts (`t` cycles): tree, review (to-review vs a folded viewed bucket,
  membership derived from the hash-keyed viewed marks so an edited file falls
  back into to-review), and kinds (below), plus walkthrough (below) where the
  review has at least one. Every group header carries the
  `+A -B` of what it holds, the file row's own diffstat summed over its files
  and right-aligned in the same column, so a folded group still says how big it
  is; a header knows its name and not its members, so the sums come from one
  pass per frame over the layout on screen. Inside a group, a directory or a
  section alike, the files already viewed sort to the top, so what is left to
  read is one run at the bottom the way the review layout's buckets do it.
  `m` marks the file and moves to the row listed under it, walking what the
  sidebar shows: a folded group stays folded, and reaching the end of the list
  with files left says so. With nothing below it the cursor holds its row
  rather than following the file, which has just sorted to the top of its
  group, so marking upward from the bottom keeps the reader where they were.
  `[`/`]` step the sidebar's headers, folder to folder or section to section,
  the way they step the status screen's groups; every bracket motion, there and
  in the diff, walks rows through one `step_to`. On a header, `m` covers everything the header stands
  for, a directory's whole subtree or a kind's whole bucket, and a second press
  puts it all back. The members come from the grouping, never from the rows, so
  a folded Generated marks the files it hides. `u` hunts the next unviewed anywhere, reading
  `DiffView::display_order`, the same order with nothing folded. Both read the
  sidebar's order, since the diff's file order is a different order on screen.
  The status screen keeps the flat magit list. OSC52 clipboard works over
  ssh/tmux.
- **Kinds sidebar.** `classify::Rules` buckets a path into one fixed set,
  Source / Tests / Docs / Config / Build & CI / Generated / Assets / Other:
  the reader's `[classify]` globs, then what the repo declares, then the
  built-in table. The table's order is the design, Generated ahead of Tests so
  a generated fixture reads as noise, and every rule reads the path alone, so
  a row build costs no IO. Buckets with nothing in them contribute no header,
  Generated and Assets start folded, and both grouped layouts share one
  `BTreeSet<Bucket>` of folds and one `section_rows` emitter (depth 0 header,
  depth 1 file, which the renderer's indent reads). What the repo declares is
  `linguist-generated`/`-vendored`/`-documentation`, read through
  `Vcs::attr` with the mapping in `classify::declared`, so the backend stays
  free of sidebar policy. That lookup walks the attribute files per path
  (~75µs each, measured), so it is a worker like any other read: `queue_declared`
  on open, on `t`, and after a refresh that moved the file list, answering as
  `AppEvent::DeclaredKinds` with a token that drops an answer for a list the
  view has replaced. Until it lands the sidebar groups by the table alone.
- **File view and blame.** `Vcs::blame` returns line runs, one per commit,
  remapped onto the worktree buffer so an edited file attributes its committed
  lines correctly and its new ones to nobody. `Review::compute_file` opens its
  own backend like `compute_refresh`, so the read, the blame and the highlight
  all run on the blocking pool and the screen opens rendered. One screen serves
  both jobs: `Screen::File` is the file viewer, and `b` toggles its blame
  column, because a viewer and a blame view differ by one column. `]`/`[` step
  commit runs, `<cr>` reviews the commit that wrote the cursor line. The gutter
  prints a commit only on the first line of its run. Reached with `B` on the
  file under the cursor (status or diff), or `gf`, the fuzzy picker over
  `Vcs::tracked_files`: the diff screens list only changed files, so the picker
  is the one way to a file the review does not touch, and it also sends one
  straight to `$EDITOR`.
- **Status bands.** The branch band leads with the branch's own PR when it has
  one, since that is what the branch is for, then the repo's walkthroughs
  when it has any (a header counting them, one row per walkthrough, folded
  like any other group), then the working-tree sections,
  Unpushed (commits no remote-tracking ref contains, walked to `UNPUSHED_LIMIT`
  and counted `N+` at the ceiling), and Recent commits. A bare
  rule then opens the repo band: Branches, Open pull requests (fetched the
  first time the group unfolds), CI runs. A branch listing resolves no
  upstream: one costs a config read and a graph walk (~2ms measured, so 570
  branches cost 1.5s on every refresh), and only the rows the section renders
  ask `Vcs::divergence` for theirs. A group is present when the repo can
  have the thing at all, so zero is an answer and only a repo without remotes
  loses its Unpushed section. `[`/`]` step group headers, `tab` folds one;
  a commit carrying CI runs takes a `▸` between its glyph and sha and unfolds
  them beneath it. `y` copies whatever the cursor addresses in the form you
  would paste: a pull request as its forge URL, a commit as its full sha, a file
  or a folder as its repo-relative path (a hunk header and a line inside an
  expanded diff both address their file, the way the editor jump reads them),
  and the branch-checkout key on a listed pull request checks that one out. Every
  async arrival (CI poll, PR fetch, watcher refresh)
  re-seats the cursor through `status_cursor_anchor`, keyed by identity
  (path, oid, branch name) rather than row index.
- **Language breakdown.** `language::of_path` names a path's language, reusing
  the highlighter's extension table (`syntax::registry`) so the languages
  diffler can parse are mapped in one place, with a small table beside it for
  the ones it counts without highlighting. Colours are Linguist's own hexes,
  the ones a repository page uses, lifted by `readable_on` until they clear a
  3:1 contrast with the theme's background: `#292929` JSON is invisible on a
  dark terminal and Linguist tuned that palette for a white page. Two surfaces
  read it. The status head band's `Languages` line breaks the working tree's
  churn down per language, from the diffstats the screen already sums, and
  stays hidden below two languages. `L` opens the Stats screen, a table of
  files/lines/code/comments/blanks per language that `stats::scan` fills from
  a read per tracked-or-untracked file on the blocking pool, token-guarded like
  every other worker; `s` cycles the sort, `<c-r>` counts again. The scan
  leaves out what `classify` calls Generated, lockfiles included, the way a
  repository page does and `scc` does by default, and says at the bottom what
  it left out. Comment counting is a per-language token table, not a parse: a
  line opening with a comment token is a comment, a shebang is code.
- **Which remote's CI.** A fork has two remotes for one repo, so the order
  `detect_ci_remotes` builds decides whose runs show: `ci.remote` when set,
  else the remote the branch pushes to (`head.upstream`'s first segment), else
  `origin`. The GitHub provider then names that repo on every call, `-R` for
  the `gh` subcommands and an expanded `{owner}/{repo}` for `gh api`, because
  `gh` resolves a fork to its parent when nobody tells it otherwise.
- **Create-pull-request form.** One list of rows: base, title, body, draft, then
  a `[ Create ]` and a `[ Cancel ]` button, so `j`/`k` and the pointer reach the
  buttons the way they reach a field (a blank line between them would break the
  row mapping `ListHits` does, hence none). Title and body both edit inline
  through the input modal, which is already multiline, and `e` hands either to
  `$EDITOR`; the base opens a branch list and draft is a toggle, so both decline
  the editor. A created PR is seated into the branch band by `seat_branch_pr`,
  since the band resolves its PR once per branch and would otherwise stay empty
  until a checkout re-armed the poll.
- **Config.** TOML, XDG-layered (built-in defaults → `~/.config/diffler/config.toml`
  → `<repo>/.diffler/config.toml` → CLI flags; every flag has a config key).
  `diffler config --dump` prints the merged config with origins.
- **Walkthrough.** The agent that made a change is the only party who knows the
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
  Option<String>, rev: Option<String> }`: `stops` is the primary comment ids
  in reading order, `summary` is the walkthrough's own overview, a markdown
  body exactly like a stop's but with no comment or anchor behind it, `None`
  for a walkthrough with none, and `rev` is the full oid of `HEAD` at publish
  time, `None` for a walkthrough saved before that field existed. A
  walkthrough source's session holds exactly one
  (`Session::walkthrough: Option<Walkthrough>`); since the source is the
  walkthrough's own, every comment in that session is this walkthrough's, its
  stops, their notes, and any human reply, with nothing left to track
  ownership of. `publish_walkthrough`
  (`diffler_core::walkthrough`) targets the source `id` names, or a fresh
  random id when it is omitted. That is what gives a stop a thread the human
  answers in, a row in the comments pane, and the card renderer, with no
  second comment system beside the first. `publish_walkthrough` materialises
  each stop as a comment (`author: agent`, `anchor.file` from the ref, or with
  no ref the first stop's file that has one, else the first file in the
  working tree, which a walkthrough always tracks)
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
  a walkthrough by id installs its own source's diff even over a clean
  working tree (`App::open_walkthrough_diff`,
  the one caller `install_diff_view` lets through empty), since the
  walkthrough's own context files fill the pane once anchors resolve, and its
  diff model is the working tree's, the same one `WorkingTree` reads; leaving
  the layout with `t` when the diff itself carries nothing cycles back to it
  rather than to tree/review/kinds, which would list nothing. The layout
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
  all work; a `mermaid` fence becomes a figure instead, static here and drawn
  once per frame into the card's rows, which every agent comment gains and not
  only a stop. Parsed bodies are cached on `DiffView` keyed by comment id, or
  by `summary_figure_key(&walkthrough.id)` for the summary's own body, and a
  hash over the body and the wrap width, so a diagram is parsed when it
  changes and not per frame; the same key scheme carries into the per-frame
  figure rasteriser, so a summary's figure draws through the exact path a
  stop's does. `graph::mermaid` takes the `flowchart`
  subset the layered engine can draw and simplifies the rest, since an agent
  that gets a rejection it cannot fix is worse off than a reader looking at a
  box where a diamond was; what was simplified comes back in the tool's reply,
  so the agent learns and the reader never sees a gap. Only a diagram with no
  node-and-edge shape at all fails, and its source stays in the body as prose.
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
  reads each file through `Review::compute_walkthrough_files`, which tries
  `rev` first (`Vcs::read_at`) and falls back to the live worktree for a
  path that revision lacks, or for a walkthrough saved before `rev` existed,
  which carries none at all; `WalkthroughRequest` carries `read_rev`
  alongside the files to read, so the worker knows which tree to open.
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
- **MCP (rmcp, streamable HTTP).** Tools: `review_status`, `get_diff`,
  `get_comments`, `list_reviews`, `reply_comment`, `propose_resolve`,
  `mark_viewed`, `wait_for_feedback`, `publish_walkthrough`, `get_walkthrough`.
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
- **PR review.** `ReviewSource::Pr{number}` keys review state on the PR number
  (survives pushes); the diff is `merge-base..head` via `Vcs::tree_diff`,
  fetching `refs/pull/<n>/head` when the head isn't local: reviewing never
  needs a checkout. The branch's PR is a status row; `b p` lists all open PRs
  (Enter reviews, `b` checks out). Forge review comments sync into the session
  (`remote_id` marks forge-owned rows); local comments and replies post back
  through queued workers (GitHub via `gh`, GitLab via `glab api`, Forgejo over
  its REST API). A Forgejo thread has no handle of its own, so it is the
  comments sharing a review, a path and a signed line, rooted at the lowest id;
  the forge exposes no resolution API, so `Capabilities::resolve_threads` is
  false there and a resolve stays in the local session.
- **GitLab merge requests.** A thread is a discussion and a comment is one of
  its notes, so a reply, an edit and a delete all route through the discussion
  the note belongs to, which `discussion_of` looks up. An anchored note repeats
  the merge request's `diff_refs` (base, start, head) plus the line, and a
  multi-line one adds a `line_range`. Writes travel as multipart form fields:
  GitLab's REST layer unflattens `position[new_line]` into nested parameters,
  which a JSON body never gets. A submitted review is draft notes plus one
  `bulk_publish`, so the author is notified once; the verdict maps onto
  approve/unapprove, the only review state the REST API records.
- **Non-goals.** Worktree/workspace management, agent orchestration,
  structural diff, task tracking.

## Distribution

- **Cut a release:** `just release-patch | release-minor | release-major`
  (`scripts/release.sh`). It prechecks (on main, clean tree, in sync with origin,
  tag free), bumps the version in lockstep across `Cargo.toml` (workspace + the
  `diffler-core` dep), `npm/diffler`, and `npm/diffler-mcp`, runs `just ci`, then
  commits, tags `vX.Y.Z`, and pushes. The version lives in the manifests; the tag
  mirrors them.
- **CI does the rest** (`.github/workflows/release.yml`, tag-triggered) via
  **OIDC trusted publishing (no stored tokens)**: build 6 prebuilt targets →
  publish the GitHub release → crates.io (`diffler-core` + `diffler`) + npm
  (`@mattfillipe/diffler` binary wrapper + `diffler-mcp` proxy). The
  `package-managers` job renders + commits Homebrew (`Formula/`), Scoop
  (`bucket/`), AUR (`packaging/aur/`), and the Nix `flake.nix` (validated with
  `nix build` before committing).
- **AUR push is manual:** `just aur-publish` (`scripts/aur-push.sh`) with your
  local AUR SSH key.
- **Channels:** crates.io, npm ×2, GitHub releases, cargo-binstall, Homebrew tap,
  Scoop bucket, AUR (`diffler-bin`), Nix flake.
