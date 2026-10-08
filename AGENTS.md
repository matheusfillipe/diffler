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
  jj.rs                JjVcs: composes the git2 backend for reads, shells out to `jj` for writes
  repo.rs              repository discovery and backend selection (git vs colocated jj)
  model.rs diff.rs     diff model, hunks
  diffalgo.rs          selectable line-diff algorithms (myers/minimal/patience/histogram/structural)
  pairing.rs           similarity line-pairing + grapheme intraline emphasis
  syntax/              tree-sitter language registry + AST-diff intraline emphasis + scope index
  highlight.rs         syntect whole-file highlight
  source.rs review.rs  ReviewSource + per-source review state
  session.rs           comments (a walkthrough stop is one) + viewed marks
  walkthrough.rs       the agent's reading order: the stops' comment ids, anchors
  store.rs             .diffler/ persistence
  lens.rs              symbol lens model: a line's names, how far each reaches, their uses
  feedback.rs          markdown feedback export

crates/diffler/        binary (color-eyre at the top; thiserror for typed errors):
  ui/ app/ tree.rs     ratatui TUI: screens, file sidebar, state
  app/composer.rs      in-place comment editor (app/text_edit.rs is its key set)
  ci/                  forge seam: CI acquisition + PR review (ForgeProvider trait; gh/glab/Forgejo REST)
  graph/               navigable orthogonal node-graph ratatui component
                       (mermaid.rs parses the flowchart subset an agent writes;
                       sequence.rs and callstack.rs are the other two figure
                       kinds a walkthrough card can draw, drawing.rs picks
                       between all three by a fence's own language and header)
  keymap.rs config.rs  configurable keybindings, layered TOML config
  theme.rs transient.rs  rendering theme, popup/modal model
  mcp.rs               rmcp/axum MCP server
  watch.rs             notify filesystem watcher
  editor.rs clipboard.rs  $EDITOR suspend/restore, OSC52 yank
  text.rs               display-width text shaping shared by the UI and the graph engine
```

## Commands (just; see `just --list`)

- `just check`: clippy with ci's denials, run after every change
- `just test`: nextest + doctests (needs `jj` on PATH for the jj backend's integration tests)
- `just fix`: clippy --fix + fmt
- `just snap`: insta snapshot tests; read `.snap.new` diffs before `just snap-accept`
- `just e2e`: PTY end-to-end suite (needs `uv` and `jj`; CI runs it in a separate job)
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

- **Layering.** Nothing above the `Vcs` trait may import git2 or shell out to
  `jj`. Two backends exist: `git.rs`'s git2 backend, and `jj.rs`'s `JjVcs`.
  `repo::open` picks between them by whether the discovered root has a `.jj`
  directory beside `.git`.
- **jj support.** Colocated repos only: a `.jj` directory beside `.git`, what
  `jj git init` makes by default. In a colocated repo git's HEAD sits on
  jj's `@-` and the worktree matches `@`, so `JjVcs` delegates every read
  (diff, log, blame, `read_at`, tree diffs) to `GitVcs` unchanged. Writes
  shell out to `jj`: commit -> `jj commit`, extend -> `jj squash -u` (a bare
  squash opens an editor when both sides carry a description), amend ->
  `jj squash` with the message, reword -> `jj describe -r @-` (touches only
  `@-`'s description, never the working copy), branch create/delete ->
  `jj bookmark create -r @` / `jj bookmark delete exact:`, checkout ->
  `jj new <rev>` (checkout keeps working on top of a branch; `jj edit`
  edits its tip commit in place), discard -> `jj restore
  root-file:<path>`. A message travels as `--message=<text>`; a name jj
  would otherwise resolve as a revset or fileset expression (a checkout's
  target, a deleted bookmark, a discarded path) travels as a quoted jj
  string literal after `--`, since a leading `-` reads as a flag and git
  allows names (`fix(x)`, `u@v`) that parse as fileset or revset syntax
  there. A created bookmark's name is never resolved as an expression, so
  it goes through unquoted. Every call sets `JJ_EDITOR=false`; a prompt jj
  opens anyway then fails at once, keeping the write's UI thread
  responsive. Staging, unstaging, hunk staging, and stash have no jj
  equivalent; each returns a `VcsError::Rejected`, and the UI shows it as a
  status message. In-app fetch runs `jj git fetch` (or, fetching every
  remote, `jj git fetch --all-remotes`) through `network_argv`; push and
  pull decline there, since the git CLI would move HEAD and branches behind
  jj's back; a PR checkout fetches the head ref with git and switches
  through `Vcs::checkout`. `Vcs::status` reads
  `working_tree_diff` (one diff pass, `@-` against the whole working copy)
  into the `staged` section: jj's colocation
  snapshot marks a new file intent-to-add in the git index, so merging
  git's own untracked/unstaged/staged lists would double-count it, git2
  reporting it once as added (tree vs index) and again as modified (index
  vs workdir). The status screen folds to that one section
  (titled "Working copy (@)") and its hint line drops `s stage`; nothing else
  about the screen forks for jj. `repo::discover` reports a jj repo with no
  `.git` at all (`jj git init --no-colocate`) as `RepoError::JjNotColocated`,
  naming `jj git colocation enable` as the fix. The watcher ignores `.jj/`
  the way it ignores `.git/objects`: a jj command rewrites its operation log
  and working-copy snapshot on every invocation, but the same command also
  moves the `.git/refs`/`.git/HEAD` it exports to, which carries the real
  signal. `head()` reads git HEAD as-is (often detached, since jj never
  moves a bookmark for you): resolving `@`'s own bookmark or change id on
  every refresh would cost a `jj` subprocess call on the UI thread for a
  cosmetic label, so `head()` skips it.
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
- **Diff pipeline.** Hunks (git2's own, or imara-diff's for histogram/
  structural, see Diff algorithm below) → similarity line-pairing → grapheme
  intraline emphasis → syntect whole-file highlight sliced onto diff lines →
  composite (syntax-fg over diff-bg over emphasis-bg). GitHub-dark default
  theme; progressive render (a plain first frame is fine).
- **Diff algorithm.** `diffler_core::diffalgo::DiffAlgorithm` (myers, minimal,
  patience, histogram, structural) is the line-diff algorithm every source
  honours: `GitVcs` carries it (plus the indent heuristic) as a `Cell`, so a
  live switch reaches the review's own long-lived backend, while every
  worker that opens a fresh one takes it in the same `DiffSettings` value
  as `context_lines`, and `App::new` pushes the configured algorithm into the
  review it is handed, so the two start equal. Myers/minimal/
  patience are git2's own `DiffOptions` flags, applied at every
  `DiffOptions::new()` site through one `apply_git_algorithm` helper.
  Histogram has no libgit2 implementation: `GitVcs::imara_hunks` re-derives
  a modified file's hunks through `diffalgo::histogram_hunks` (imara-diff,
  already linked in as `syndiff`'s own line-diff engine), mirroring git's own
  hunk-merging rule and header numbering. It reads the worktree side raw, so
  a file whose bytes hash to something other than what git compared (a clean
  filter such as autocrlf) keeps git2's hunks. Their function heading comes
  from libgit2's own default funcname rule, applied by `FuncHeading`: the
  nearest old-side line above the hunk's first row that opens in column one
  with a letter, `_` or `$`, cut to 80 bytes, so switching algorithm keeps
  the heading a hunk had under git2. Structural is histogram plus
  reformat detection: `syntax::intraline` reuses the AST diff it already
  computes for intraline emphasis, and a paired deleted/added line that
  differs in whitespace alone, with no token changed, is flagged
  `DiffLine::reformat_only` and renders as a context line: dimmed text and a
  `≈` between the gutter numbers and the text, so red, green and the rail
  stay for real changes. Python, YAML, Haskell, Make and Scala never get the
  flag, since layout is syntax there. Hunk staging re-derives the target
  file's hunks through the same
  `imara_hunks`, so the id the reviewer picked is findable; under
  histogram/structural the staged patch copies each line's bytes from the
  file's own text (`render_hunk_patch_from_model`), since the model's text
  has its line endings stripped. Both renderers share `write_hunk`: libgit2
  applies a hunk at its `+` start, so a hunk sent alone names its minus
  side's position there. A live switch (`<c-a>` on the diff screen) updates
  the config and the backend, then re-diffs the status sections and whatever
  review is open on the blocking pool (`queue_rediff`, token-guarded, answered
  by `on_rediff_done`). The re-diff holds the refresh slot while it runs, so
  it and a watcher refresh land in the order they read the repo, and it
  names the cursor's rows when it lands, keeping the cursor through
  `RowRef`'s capture/restore; an enrichment
  queued under the old algorithm is dropped on arrival, since a content hash
  cannot tell the two apart. The pane heading trails `· <algorithm>`
  whenever it isn't the default. Once the re-diff lands, the status line says
  how many files' hunks changed (compared by hunk id), or that the hunks came
  out the same, since most diffs do under every algorithm and a silent switch
  reads as a broken one.
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
  (reply, resolve, delete) reach it with no handling of their own. Every
  comment but the one under the cursor draws as one line, author then the
  body's first line elided; the cursor's own opens the full card, and only
  that open card or a group's last item trails a blank spacer, so a busy pane
  reads as a dense list rather than a wall of gaps. The author leads each row
  in a colour stepped by the golden angle from where that author first
  appears in the pane's own order, lifted through `readable_on` for contrast;
  the human's own author name and the agent's take the theme's fixed accent
  and purple, since a reader looks for those two first and neither hue is
  ever handed to anyone else. `t` in that pane cycles its own grouping,
  independent of the file sidebar's own layout: by file (diff order), by
  author (first-appearance order), by status (open, replied, resolved, the
  last folded by default), or a flat list with no headers at all. A
  grouping's headers follow the same header/count/fold shape `section_rows`
  gives the file sidebar (`group_header_line` the one they share), its items
  indented one level under, the way a file indents under its directory:
  `tab`/`za` folds the one the cursor sits in, `[`/`]` step headers, and a
  header under the cursor selects no comment, so a verb that needs one
  declines rather than reaching whatever the diff cursor was last on. A
  card sits on a faint surface of its own (`card_surface`) and its text wraps
  at `CARD_MEASURE` columns however wide the pane is. A thread draws each
  reply as its own block, an author line over the body, with a bar in the
  author's colour in place of the card's status bar; the reader's own replies
  line up under the first comment and everyone else's indent `REPLY_LANE`
  columns. A forge thread is a
  flat list of replies, so the lanes show authorship and never nesting. A
  closed thread folds the replies between its first comment and its latest
  reply into one `▸ N earlier replies` row once it has more than two, and
  always shows the latest reply in full; `tab`/`za` on any of its rows
  opens it (`DiffView::open_threads`, read by `comment_display` so the row
  count and the drawing agree). Comments,
  replies and edits are written in place: the composer occupies the rows the
  finished card will, under the anchored line, at the top of the file for a
  whole-file comment, under the thread for a reply. `<c-g>` there, and in the
  input modal behind a branch name, a PR title or body, and a review summary,
  hands the buffer to `$EDITOR` on a scratch temp file, deleted once the text
  is read back whatever the outcome, and reads it into the same box; a
  cancelled edit, a failed editor, or the box holding nothing focused all
  leave it exactly as it was, the last one saying so. Runs (the
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
  A viewed file leads with a green `✓` in place of its status glyph, the way a
  resolved comment does in the comments pane, so a run of finished files reads
  down the left edge. The lead cell stays the cursor's own `▌` alone, and a
  rail in the diff means an added line and nothing else.
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
  The diff pane folds hunks (`app/diff/folds.rs`), the same unit `]`/`[`
  step through. `za`/`<tab>` on any row of a hunk folds it, and on a folded
  hunk opens it; `zM` folds every hunk of the file and `zR` opens them all.
  Nothing starts folded. A folded hunk replaces its header and everything
  under it with one `DiffRow::Fold` that reads as the header plus what it
  hides, `@@ -19,7 +19,7 @@ fn f() {  ⋯ 7 lines +1 -1 · 1 comment`, so `]`/`[`
  (`DiffRow::is_hunk_header`) land on it folded or open. A fold is keyed by
  the hunk's id, so a hunk the agent edits comes back open with the change
  showing. An open composer's rows stay on screen inside a folded hunk.
  Motions that can land on a hidden line open its hunk through
  `DiffView::reveal_line`: `(`/`)`, and a committed search, which matches a
  fold row on the code it hides. `RowRef::Hunk` and `RowRef::Fold` each find
  the other form of the same hunk, so folding or opening one keeps the
  cursor on it.
- **Image preview.** A binary file named `.png`/`.jpg`/`.jpeg`/`.gif`/`.webp`
  (`app::image::is_image`) draws its sides as pictures in the diff pane
  (`ui/image_pane.rs`): before and after side by side, stacked below 60
  columns, or the one side an add or a delete has, each fitted and centred in
  its own frame with its pixel size and byte size under it; whole-file comment
  cards keep rows below. `FileDiff::blobs` carries each side's git blob id,
  which `sides_hash` folds in and `content_hash` returns as is for the new
  side, since a binary file has no text to hash: a rewritten image refreshes,
  and a viewed one falls back into to-review, the way an edited text file
  does. The draw records what it
  wants as an `ImageKey` (path, blob ids, fit size) and `queue_image_preview`
  turns a key that is neither on screen nor in flight into a worker request:
  `Review::compute_binary_sides` reads each side by blob (`Vcs::read_blob`),
  the worktree side from disk when the store holds no object for it, capped
  at `MAX_PREVIEW_BYTES` a side, then `app::image::build_preview` decodes it
  and encodes it for the terminal at that size, scaling a small image up with
  nearest-neighbour so its pixels stay readable. The protocol comes from
  ratatui-image's `Picker::from_query_stdio`, asked once in `run` after raw
  mode and before the event pump reads stdin: kitty (Unicode placeholders,
  which survive a multiplexer), sixel or iTerm2 when the terminal answers,
  halfblocks otherwise.
- **Symbol lens.** `*` in the diff pane (`app/diff/lens.rs`, over the pure
  model in `diffler_core::lens`) names every identifier on the cursor line,
  from one parse per side (`LanguageRegistry::symbols`, plain words where a
  language has no grammar), up to nine, tints each one's uses in its own golden-angle colour
  through the same `Mark` path search hits take, and draws each name's digit
  in red over its first letter on that line, so the number that picks a name
  sits on the name. A name the parse treats as an item on that line (a
  definition, a call or method call the tags query marks, or a type position)
  reaches every file of the diff, both sides, whether or not the diff defines
  it; any other name is a local and reaches only its enclosing function
  (`ScopeIndex::enclosing`, held to that one function's span even when the
  file defines another of the same name), or its file at the top level. Uses count only on
  lines the diff shows, a deleted line on its old side and an added or context
  line on its new side, so a context line counts once. Building it parses both
  sides of every file, so it is a worker like enrichment (`pending_lens` →
  `AppEvent::Lens`, its token held by the view that asked, so an answer for a
  replaced view is dropped). `*` again cycles the focus through the
  names and back, `#` the same ring backwards (vim's pair), a digit picks one the way a
  numbered list does, and `esc` closes it. A focused name opens the references sidebar in the comments
  sidebar's place (`Pane::References`; opening either puts the other away): its
  uses in diff order, one per line, grouped by file under a header that dims
  the folder and cuts it from the front so the file name always shows, each a
  few lines of the hunk around it with the diff's `-`/`+` markers, their shared
  indent cut, highlighted from the file's enrichment (queued for every listed
  file) and the name tinted. A reference is keyed by its path, side
  and line number and found again when it is seated, so expanding context or
  re-diffing never points it at the wrong row. The keyboard stays in the diff when it
  opens; `l` moves into it, and moving its selection seats the diff on that use
  through `seat_ref`, switching file and opening a fold as needed, the way the
  comments sidebar's selection drives the diff cursor; paging, `[`/`]` (file to
  file), the wheel and a click work there as they do in the comments sidebar. `n`/`N` step that same
  selection when a name is focused, and walk every tinted use in diff order when
  none is. Side-by-side draws no lens and `*` there names the unified view. A
  refresh that moves the model drops the lens, since its line numbers name the
  old text.
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
  expanded diff both address their file, the way the editor jump reads them);
  over a `V` selection it copies every row the selection covers, one per line,
  each value once, and the branch-checkout key on a listed pull request checks that one out. Every
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
- **Matrix legs on the run graph.** A `CiJob` carries `legs`, populated by the
  provider when a `strategy.matrix` fanned a YAML job out into several run
  jobs: GitHub's `expand_jobs` still folds every matching run job into one
  `CiJob` (one node per YAML job, not per leg), but now keeps each leg's own
  name, status and duration instead of losing them to the aggregate. A leg is
  recognized by its run job name starting with `<job name> (`, GitHub's own
  matrix naming, never by parsing or counting the matrix parameters, since a
  leg whose matrix value is empty drops that parameter from the name (`build
  (Dockerfile.cuda, -cuda)` beside `build (Dockerfile.gpu)`). `to_model` turns
  a job with legs into a foldable group: the job keeps its node id as the
  group's root (`Node.foldable`), and each leg becomes a member node
  (`Node.group`) with the matrix parameters alone as its label, the job's own
  name already on the root. A leg's node id is the run job it ran as
  (`CiJobLeg.id`, e.g. `build (3.11)`), so `<cr>` on a leg opens that leg's
  own log through `job_log`'s exact-name match. A `needs` edge is always resolved at the job
  level, so it lands on the root, never a leg. Forgejo's provider maps each
  task straight into its own `CiJob` with no per-job grouping step at all (no
  workflow YAML is parsed there), so a Forgejo matrix already shows one plain
  node per leg, just not folded under a shared root; GitLab's `parallel:`
  jobs have the same gap and are untouched for the same reason.
- **Create-pull-request form.** One list of rows: base, title, body, draft, then
  a `[ Create ]` and a `[ Cancel ]` button, so `j`/`k` and the pointer reach the
  buttons the way they reach a field (a blank line between them would break the
  row mapping `ListHits` does, hence none). Title and body both edit inline
  through the input modal, which is already multiline, and its own `<c-g>`
  already reaches `$EDITOR` once a field is open; the form keeps a direct `e`
  as a shortcut past that step, straight from the row, through the same
  scratch-file mechanism, so the base and the draft toggle still decline (not
  text) while title and body skip typing `<cr>` first. A created PR is seated
  into the branch band by `seat_branch_pr`,
  since the band resolves its PR once per branch and would otherwise stay empty
  until a checkout re-armed the poll.
- **Config.** TOML, XDG-layered (built-in defaults → `~/.config/diffler/config.toml`
  → `<repo>/.diffler/config.toml` → CLI flags; every flag has a config key).
  `diffler config --dump` prints the merged config with origins. `[diff]
  algorithm`/`indent_heuristic` set the line-diff algorithm (see Diff
  algorithm, above). `[ui] show_agent_activity` toggles the
  status bar's live agent indicator (see MCP, below).
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
  Option<String>, rev: Option<String>, about: ReviewSource }`: `stops` is the
  primary comment ids in reading order, `summary` is the walkthrough's own
  overview, a markdown body exactly like a stop's but with no comment or
  anchor behind it, `None` for a walkthrough with none, `rev` is the full oid
  of `HEAD` at publish time, `None` for a walkthrough saved before that field
  existed, and `about` is the review this walkthrough describes: the working
  tree, or the commit/range/PR the human had open when it was published,
  defaulting to the working tree for a walkthrough saved before this field
  existed. A walkthrough source's session holds exactly one
  (`Session::walkthrough: Option<Walkthrough>`); since the source is the
  walkthrough's own, every comment in that session is this walkthrough's, its
  stops, their notes, and any human reply, with nothing left to track
  ownership of. `publish_walkthrough`
  (`diffler_core::walkthrough`) targets the source `id` names, or a fresh
  random id when it is omitted. That is what gives a stop a thread the human
  answers in, a row in the comments pane, and the card renderer, with no
  second comment system beside the first. `about` is whichever review the
  human has open at publish time (what `review_status` already reports), so
  no separate argument names it; revising a walkthrough while looking at its
  own diff keeps whatever it already described, since that source names no
  review of its own to fall back on. `publish_walkthrough` materialises
  each stop as a comment (`author: agent`, `anchor.file` from the ref, or with
  no ref the first stop's file that has one, else the first file of the
  review it is about)
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
  a walkthrough by id (`App::open_walkthrough_diff`) resolves `about`
  (`App::walkthrough_about`) and installs its own source's diff over that
  review's own model (`App::source_model`, which recurses into `about` for a
  `Walkthrough` source), the working tree's when `about` is one, even over a
  clean working tree (the one caller `install_diff_view` lets through empty),
  since the walkthrough's own context files fill the pane once anchors
  resolve; leaving the layout with `t` when the diff itself carries nothing
  cycles back to it rather than to tree/review/kinds, which would list
  nothing. The layout
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
  a C/C++ method defined outside its class answers to `Class::method` too,
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
  all work; a ` ```mermaid ` or ` ```callstack ` fence becomes a figure
  instead, static here and drawn once per frame into the card's rows, which
  every agent comment gains and not only a stop. A figure is a
  `graph::Drawing`: `Graph`, a navigable flowchart, or `Text`, a sequence
  diagram (`graph::sequence`) or callstack tree (`graph::callstack`) laid out
  once at parse time into rows of styled text (`graph::text_figure`).
  `graph::figure` picks between them: a ` ```callstack ` fence is always a
  callstack; a ` ```mermaid ` fence is a sequence diagram when its first line
  names a `sequenceDiagram`, else a flowchart through `graph::mermaid` (the
  `flowchart` subset the layered engine can draw, simplified where it cannot,
  since an agent that gets a rejection it cannot fix is worse off than a
  reader looking at a box where a `((circle))` was). A text figure lays out to the
  card's width, so a resize re-lays it out through the same width-keyed
  cache: a sequence diagram widens the gap before each message's right end
  until its label fits, left to right, while the width lasts, and elides the
  label to its lane after that; a callstack elides each label to its row.
  Neither is cropped at `FIGURE_MAX_ROWS`, since there is no full screen to
  show the rest, and their own caps (`MAX_EVENTS`, `MAX_FRAMES`) bound them.
  A mermaid `subgraph` tags its members with its outermost id, and a node
  first named on an edge joins the first subgraph that lists it. Once laid
  out, the engine outlines a subgraph only when its members land contiguous,
  no foreign node inside their margin and no other outline overlapping; the
  layout reserves that margin only when the model carries a subgraph, so an
  ordinary flowchart pays nothing for it. Outlines draw before the edges, so
  a crossing edge merges into a junction and keeps its arrowhead, and the
  title goes on whichever border has room clear of an arrowhead. A
  `{decision}` node draws with a `◇` marker and earns none of the "drawn as a
  box" notes the other shapes the engine cannot render get. What was
  simplified comes back in the tool's reply, so the agent learns and the
  reader never sees a gap; only a mermaid diagram with
  no node-and-edge shape at all (and no `sequenceDiagram` header) fails, and
  its source stays in the body as prose. Parsed bodies are cached on
  `DiffView` keyed by comment id, or
  by `summary_figure_key(&walkthrough.id)` for the summary's own body, and a
  hash over the body and the wrap width, so a diagram is parsed when it
  changes and not per frame; the same key scheme carries into the per-frame
  figure rasteriser, so a summary's figure draws through the exact path a
  stop's does. `o` opens a `Graph` figure full screen; a `Text` figure has no
  full screen to open, so `o` on one says as much and names `<cr>`: on a
  figure row whose drawing names a resolved node (a callstack frame's own
  anchor, or either row of a message to a participant that mermaid's own
  `link <participant>: <label> @ <target>` statement anchors), `<cr>` in the
  diff pane jumps straight to that code, through the same `resolved` map a
  graph's `click` targets fill.
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
  reads each file through `Review::compute_walkthrough_files`, from the copy
  `ReadFirst` names and else the other: `rev` (`Vcs::read_at`) first for a
  walkthrough about a commit, a range or a PR, and the worktree first for one
  about the working tree, since that code was never in the pinned commit and
  `rev` only serves a file deleted since. A walkthrough saved before `rev`
  existed reads the worktree alone. `WalkthroughRequest` carries `read_rev`
  and `read_first` alongside the files to read. A file the diff itself
  carries skips that read when its anchors resolve: they resolve against the
  diff's new side, the text the slide shows.
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
  `mark_viewed`, `add_comment`, `delete_comment`, `edit_comment`,
  `report_activity`, `wait_for_feedback`, `publish_walkthrough`,
  `get_walkthrough`. `add_comment`
  writes a new comment on a line or an inclusive line range of the review the
  human is currently looking at, anchored exactly the way a human's own
  comment is; authored as the agent by default, or as the human when
  `as_human` is set, so it goes out untouched with their next submitted
  review. `delete_comment` and
  `edit_comment` are the agent's only way to take one back or rewrite it, so a
  comment that has drifted or turned out wrong need not sit answered by
  another comment stacked on top; both refuse a comment they did not author
  (`comment.author`) and a walkthrough stop or note (`comment.anchor_ref` set)
  regardless of author, since those two live and die with
  `publish_walkthrough`, which already tracks their ids and threads across a
  revision. `delete_comment` also refuses one someone else has replied to,
  since a reply lives inside its comment and would otherwise disappear with
  it unseen; `edit_comment` never touches replies, so it carries no such
  refusal.
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
- **PR review.** `ReviewSource::Pr{number}` keys review state on the PR number
  (survives pushes); the diff is `merge-base..head` via `Vcs::tree_diff`,
  fetching the head when it isn't local, from the ref the forge serves it under
  (`refs/pull/<n>/head`, or `refs/merge-requests/<n>/head` on GitLab): reviewing never
  needs a checkout. The branch's PR is a status row; `b p` lists all open PRs
  (Enter reviews, `b` checks out). Forge review comments sync into the session
  (`remote_id` marks forge-owned rows); local comments and replies post back
  through queued workers (GitHub via `gh`, GitLab via `glab api`, Forgejo over
  its REST API). A Forgejo thread has no handle of its own, so it is the
  comments sharing a review, a path and a signed line, rooted at the lowest id;
  the forge exposes no resolution API, so `Capabilities::resolve_threads` is
  false there and a resolve stays in the local session.
  A comment anchored to a whole file rather than a line (`NewPrComment.line:
  None`) needs `Capabilities::file_comments`: GitHub and GitLab have one, so
  it rides along with a submit; Forgejo has none, so `pr_pending` holds it
  back in `PrPending::file_level`, and the submit confirmation names the
  forge rather than blaming reviews in general. GitHub's own review-comments
  array has no slot for a line-less entry at all, so `GitHubProvider::
  submit_pr_review` posts each whole-file comment through `post_pr_comment`
  (`subject_type=file`, no line) once the batched review lands: one more
  network call and one more forge notification per whole-file comment,
  since GitHub gives no way to fold it into the single review post.
- **GitLab merge requests.** A thread is a discussion and a comment is one of
  its notes, so a reply, an edit and a delete all route through the discussion
  the note belongs to, which `discussion_of` looks up. An anchored note repeats
  the merge request's `diff_refs` (base, start, head) plus the line, and a
  multi-line one adds a `line_range`; a whole-file comment carries the same
  refs with `position[position_type]=file` and no line, GitLab's own read on
  a file-level note, so it drafts and publishes through the same batch a line
  comment does. Writes travel as multipart form fields:
  GitLab's REST layer unflattens `position[new_line]` into nested parameters,
  which a JSON body never gets. A submitted review is draft notes plus one
  `bulk_publish`, so the author is notified once; the verdict maps onto
  approve/unapprove, the only review state the REST API records.
- **Non-goals.** Worktree/workspace management, agent orchestration,
  a difftastic-style structural layout (the structural algorithm keeps the
  line-based layout), task tracking.

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
