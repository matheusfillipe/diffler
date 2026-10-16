# Diff pipeline

## Diff pipeline

Hunks (git2's own, or imara-diff's for histogram/
structural, see Diff algorithm below) → similarity line-pairing → grapheme
intraline emphasis → syntect whole-file highlight sliced onto diff lines →
composite (syntax-fg over diff-bg over emphasis-bg). GitHub-dark default
theme; progressive render (a plain first frame is fine).

## Diff algorithm

`diffler_core::diffalgo::DiffAlgorithm` (myers, minimal,
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

## Grammars

`syntax::registry::REGISTRY` is one process-wide `LazyLock`
holding every bundled grammar; a language compiles its highlight query on
first use (~15ms) behind a `OnceLock`, on the enrichment thread. Registering
a grammar is therefore free until someone opens that language, and a theme
switch reuses the compiled queries. Some grammars extend another (`cpp` over
`c`, `svelte` over `html`, `tsx` over `js`+`ts`): register the concatenation
or the query silently matches almost nothing. `every_language_colours_a_sample`
in `highlight.rs` is the guard.
A path finds its grammar in this order: the reader's rules, the exact file
name or the extension (`LanguageRegistry::for_path`), then, for
highlighting (`for_file`), a known name with a suffix (`Dockerfile.prod`,
`.env.local`), the extension under a template suffix
(`config.yml.example`), and the interpreter a `#!` line names. The kinds
sidebar and the Stats screen read `for_path` alone, so a backup or a
sample file keeps its kind. The rules live on each tab's own `Highlighter`
(`with_rules`), so each project's rules apply to its own files: the `gl`
picks for this run, newest first, then `[syntax]` globs, the ones without
a wildcard first and then the longer before the shorter. A rule change
rebuilds the highlighter the way a theme switch does
(`App::rebuild_highlighter`): it reloads the file view through its worker,
keeping the cursor, and `EnrichStamp` drops any enrichment the old one ran.
`gl` opens the choice picker on the file's current language, then asks
how long the pick holds; saving it for the file drops a pick made for this
run. The saved scopes write the rule into the project's
`.diffler/config.toml` (`config::save_syntax_rule`), editing the
`[syntax]` section line by line and refusing an edit that would not parse
back to the rule. The rules reach highlighting, scope breadcrumbs,
intraline emphasis, the symbol lens and walkthrough anchors; a rule change
drops an open lens and resolves the anchors again. The kinds sidebar and
the Stats screen read the registry alone.
