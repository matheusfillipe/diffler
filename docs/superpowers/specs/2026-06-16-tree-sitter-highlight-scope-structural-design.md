# Tree-sitter foundation: highlight, scope context, structural diff

Date: 2026-06-16
Branch: `feat/tree-sitter`

## Summary

Adopt tree-sitter as a shared parsing foundation in `diffler-core` and build
three features on top of it:

1. **Highlight** — replace the syntect highlighter with `tree-sitter-highlight`,
   keeping the existing `Vec<Vec<StyledRange>>` output contract.
2. **Scope context** — a breadcrumb of the enclosing definitions ("what are we
   inside of") for the line at the top of the diff pane, rendered as a sticky
   header and used to enrich hunk section headings.
3. **Structural diff** — char-precise intraline change emphasis whose line
   pairing and format-noise suppression can be driven by an AST-aware engine
   (syndiff), with the existing textual engine kept as a configurable fallback.

All three share one parse per file. Grammars for all bundled languages are
statically linked (no runtime download — musl-static binaries cannot `dlopen`).
Expected binary size ~28 MB (up from ~8 MB); accepted.

## Goals

- Better, more accurate syntax highlighting across many languages from one engine.
- Always show what scope the cursor/top-of-view is inside of, in any bundled
  language.
- Highlight exactly which characters changed in a line, with AST-aware pairing
  when a parse is available and a textual fallback when it is not.
- Be failsafe: parsing never blocks the render loop and never panics; any
  failure degrades silently to plain/textual behavior.

## Non-goals

- No LSP. tree-sitter (highlight + tags + structural) covers what this work
  needs; cross-file semantic features remain a possible far-future, opt-in track.
- No full difftastic-style AST re-layout. Layout stays line-based; the
  structural engine improves pairing, noise suppression, and emphasis precision
  only.
- No runtime grammar fetching/compilation.
- No diagnostics gutter.

## Decisions (locked during brainstorming)

- Highlight engine: **official `tree-sitter-highlight`** (stays at MSRV 1.88,
  shares the grammar set with scope + structural, full control over queries).
  Rejected: `syntastica`/`lumis` batteries crates (separate grammar set; `lumis`
  needs MSRV 1.91).
- syntect: **replaced fully.** Drop `syntect` + `two-face`. Languages without a
  bundled grammar render plain.
- Structural diff: **syndiff now**, isolated behind a trait so it is swappable.
- Grammar set: **bundle all** (static link), accept ~28 MB.

## Architecture

New module tree in `diffler-core`: `syntax/`.

```
crates/diffler-core/src/syntax/
  mod.rs        registry + ParsedFile + failsafe parse entry point
  registry.rs   LanguageRegistry: path/ext/filename -> LangSpec
  highlight.rs  tree-sitter-highlight -> Vec<Vec<StyledRange>> (replaces the
                old top-level highlight.rs)
  theme.rs      capture-name -> StyledRange style maps for the 3 syntax themes
  scope.rs      enclosing_scope(parsed, line) -> Option<Vec<Crumb>>
  intraline.rs  IntralineEngine trait + grapheme + syntactic (syndiff) impls
```

### Language registry and parse

```rust
pub struct LangSpec {
    pub name: &'static str,
    pub grammar: tree_sitter::Language,
    pub highlights: tree_sitter::Query,
    pub tags: Option<tree_sitter::Query>, // scope/definition captures
}

pub struct ParsedFile {
    source: String,
    tree: tree_sitter::Tree,
    lang: &'static LangSpec,
}

/// Failsafe seam. None when: no grammar for the path, the file exceeds the
/// size cap, or parsing errors out. Callers degrade to plain behavior.
pub fn parse(registry: &LanguageRegistry, path: &str, content: &str)
    -> Option<ParsedFile>;
```

- `LanguageRegistry` is built once (lazy), mapping extension/filename to a
  `LangSpec`. Grammar crates that expose `HIGHLIGHTS_QUERY` / `TAGS_QUERY`
  constants are used directly; missing queries are vendored under
  `crates/diffler-core/queries/<lang>/`.
- Size cap (e.g. > 2 MB or > 50k lines) → `None` to avoid pathological cost.

### Feature 1 — Highlight

`Highlighter::highlight(path, content) -> Vec<Vec<StyledRange>>` keeps its exact
signature and `StyledRange { range, fg, bold, italic }` shape, so the renderer
and the diff-background compositor are untouched.

- Internally: `tree-sitter-highlight` emits highlight events over byte ranges
  using the language's `highlights` query; events are sliced into per-line
  `StyledRange`s (trailing newlines trimmed, exactly as today).
- Themes: `SyntaxTheme` (OneHalfDark / OneHalfLight / Dracula) becomes a map
  from tree-sitter standard highlight names (`keyword`, `function`, `type`,
  `string`, `comment`, `constant`, `variable`, `operator`, `punctuation`, …) to
  a style. Defined in `theme.rs`. Colors will shift slightly vs syntect;
  accepted.
- Unknown language → one empty `Vec` per line (plain), same as today.

### Feature 2 — Scope context

```rust
pub struct Crumb { pub kind: &'static str, pub name: String }

pub fn enclosing_scope(parsed: &ParsedFile, line: usize) -> Option<Vec<Crumb>>;
```

- Walk from the node at the line's start byte up to the root; keep ancestors
  that the language's `tags` query marks as definitions (`@definition.function`,
  `@definition.class`, `@definition.method`, `@definition.module`, …). Extract
  each definition's name from its name capture. Return innermost-last.
- UI: a sticky breadcrumb line pinned at the top of the diff pane, reflecting
  the top visible line as the pane scrolls. The same lookup enriches a hunk's
  section heading where git's funcname is empty.
- Languages without a `tags` query → `None` → no scope line (failsafe).

### Feature 3 — Structural diff (char-precise change emphasis)

Layout stays per-line. Two stages:

1. **Pairing + change detection** (which old line ↔ which new line; is a line
   only reindented; is a hunk format-only). Engine-selectable:
   - `grapheme` — existing textual similarity pairing in `pairing.rs`.
   - `syntactic` — AST-aware via syndiff: align trees, derive changed byte
     ranges per side, and suppress noise. **Reindent + wrap suppression is the
     headline requirement**: when a block is wrapped in a new parent (e.g. a JSX
     subtree moved under `{({ values, isValid }) => ( … )}`) and its lines are
     reindented, the unchanged inner lines must render as *context* (not
     ±changed) even though their leading whitespace and line numbers shifted —
     so only the genuinely new/changed lines (the wrapper, and lines whose
     non-whitespace content actually differs) are signaled. Concretely: lines
     that match after leading-whitespace normalization AND whose AST nodes align
     are treated as unchanged. A purely textual diff shows the whole block as
     63−/60+; the syntactic engine must reduce that to the few real changes.
2. **Intraline emphasis** — char/grapheme-precise diff on the paired old/new
   line content. Shared by both engines; this is where exact changed characters
   are highlighted. Sharpen the existing grapheme emphasis; map syndiff byte
   ranges onto the same per-line emphasis ranges when syntactic is active.

Shape (illustrative; exact types settled during implementation): a trait that,
given the old and new file text plus their optional parses, returns the line
pairing and per-line char-precise emphasis ranges. Two implementations:
`GraphemeEngine` (textual, no parse needed) and `SyntacticEngine` (syndiff).
syndiff lives behind this trait so it can be replaced (a DIY engine or a future
crate) without touching callers; the grapheme engine is the always-available
fallback.

### Config

XDG-layered TOML (existing system), new key:

```toml
[diff]
intraline = "auto"   # auto | syntactic | grapheme
```

- `auto` (default): **semantic by default** — syntactic when a parse exists for
  both sides, else grapheme. The semantic view is what the user sees unless they
  opt out.
- `grapheme`: always the textual engine; no tree-sitter pairing.
- `syntactic`: prefer AST; still falls back to grapheme on parse failure.

syndiff's ability to collapse the reindent+wrap case is validated empirically in
Stage 3 against the JSX example above. If syndiff alone does not reduce it,
leading-whitespace-normalized line pairing (trim-equal lines → unchanged) is
added underneath the AST step — it absorbs pure reindentation cheaply and is the
fallback that guarantees the headline behavior.

Every flag has a config key per project convention; expose a matching CLI flag.

## Data flow

Unchanged shape from today's deferred pipeline. The existing async diff/highlight
worker (the tokio task that swaps `Arc` models) additionally:

1. parses old + new content once each (`parse`),
2. computes highlight, the scope index, and intraline pairing/emphasis,
3. swaps the results into the `Arc` model.

The ratatui render loop only reads precomputed results; it never parses.

## Error handling / failsafe

Any of: no grammar, parse error, oversize file, missing query, syndiff failure →
degrade silently:
- highlight → plain (empty ranges),
- scope → no breadcrumb line,
- intraline → grapheme engine,
- diff → line-based as today.

No panics, no blocking the UI, no user-facing error. This is a hard requirement.

## Testing

`diffler-core` unit tests:
- highlight: per-language output has multiple colors; theme switch recolors;
  unknown extension yields plain lines (port existing highlight tests).
- scope: `enclosing_scope` returns the expected definition breadcrumb for a line
  inside a nested function/class; `None` for unsupported language.
- intraline: syntactic engine reports no changes for a reformat-only edit;
  grapheme fallback engaged when parse is `None`; emphasis ranges are char-precise.
- **tsx/jsx reindent+wrap**: a fixture where an inner JSX block is wrapped in a
  new arrow-function parent and reindented, plus 1-2 genuinely changed lines.
  Assert the syntactic engine marks the reindented-but-identical inner lines as
  unchanged and signals only the wrapper + real changes (not the whole block).
- `parse` returns `None` on garbage / unsupported / oversize input.

`diffler` (TUI): snapshots are text-only (`.backend()`), so the color-engine
swap does not churn them. Add snapshots for the sticky scope header and the
structural toggle. Run `just snap` and review `.snap.new` diffs.

`just ci` must pass; run `just e2e` after TUI changes.

## Sequencing

Stacked commits/PRs on `feat/tree-sitter`:

1. Foundation (`syntax/` module, registry, parse, failsafe) + highlight
   (syntect → tree-sitter, drop `syntect`/`two-face`).
2. Scope context (breadcrumb + sticky header + hunk heading enrichment).
3. Structural diff (syndiff engine behind `IntralineEngine` + config).

## Risks and mitigations

- **MSRV**: verify `tree-sitter`, `tree-sitter-highlight`, all grammar crates,
  and `syndiff` build on Rust 1.88 before committing the dep set; if any
  requires > 1.88, reconsider that dep (do not bump MSRV silently).
- **Build time**: compiling many C grammars is slow on a clean build; cached
  after. Note in `AGENTS.md` if it materially changes contributor experience.
- **Binary size**: ~28 MB. Accepted. Revisit a curated/feature-gated subset only
  if a distribution channel complains.
- **syndiff maturity**: young crate (low adoption). Pin the version and keep it
  behind `IntralineEngine` so it is swappable; the grapheme engine is always a
  working fallback.
- **Color fidelity**: tree-sitter themes will not match syntect exactly. The
  three themes are re-tuned by hand against the standard highlight names.

## Dependencies (workspace)

Add to `[workspace.dependencies]`, justified in commits:
`tree-sitter`, `tree-sitter-highlight`, the bundled `tree-sitter-<lang>` grammar
crates, `syndiff`. Remove `syntect`, `two-face`.
