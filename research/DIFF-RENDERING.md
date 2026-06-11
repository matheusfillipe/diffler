# Diff Rendering — State of the Art (2026-06-11)

How to get "best diff possible": GitHub-like line bg + intra-line word/char emphasis, syntax-aware,
correct across hunk boundaries.

## How the references do it

- **GitHub**: intra-line highlight only when removed:added lines pair **1:1** in a hunk; algorithm is
  common-prefix + common-suffix, middle marked changed (deliberately not exact char LCS — one blob
  reads better than scattered edits). 2026 rewrite: virtualized rendering, O(1) comment lookup maps.
  https://github.blog/engineering/architecture-optimization/the-uphill-climb-of-making-diff-lines-performant/
- **delta**: pairs "homologous" minus/plus lines via similarity threshold (`max-line-distance` ~0.6),
  then Levenshtein edit spans within pair. syntect = foreground, diff status = background, composited
  per cell.
- **difftastic**: tree-sitter AST diff. Semantic, ignores reformatting — but poor scaling (can hang on
  140k-line files, issue #153), falls back to word/line diff on unknown langs or parse errors, and
  output doesn't map to lines ⇒ fights line-anchored comments. NOT a review-tool backbone; optional
  toggle at best.
- **Pierre/hunk** (`@pierre/diffs` + Shiki): parse diff → structured objects; render plain text
  FIRST, highlight async (workers + LRU cache); line-range virtualization with binary-search
  checkpoints; detach parsed substrings to avoid retaining whole files.

## Key decisions for diffler

1. **Diff algorithm: histogram** (extended patience). Readability, not speed — Myers produces
   "sliders" (anchors on blank lines/braces, splits unrelated code). Git community consensus.
2. **Line pairing**: similarity-threshold pairing (delta model) when counts allow; fall back to plain
   line coloring when ambiguous (GitHub's safe default). Then word/char-level diff within the pair.
3. **Highlight the WHOLE file each side, slice spans onto diff lines.** Single most important
   correctness decision: per-hunk highlighting breaks on multi-line strings/comments opened before
   the hunk. A review tool has both full files (HEAD + working tree) — use them.
4. **Composite per cell**: syntax = fg, diff add/del = line bg, intra-line edit = stronger bg/bold.
5. **Progressive render**: layout + plain text first, upgrade to highlighted async; LRU-cache
   highlighted lines; virtualize to visible range.
6. **Focused-line UX** (revdiff pattern): cursor pinned to logical line, viewport scrolls under it,
   pin-to-edge when pushed off-screen, catch-up after fast scroll.

## Stack mapping

- Rust: `similar` (Algorithm::Histogram + `iter_inline_changes`) + tree-sitter or syntect full-file
  highlight + ratatui. All mature, no shelling out.
- Go: shell to `git diff --histogram` or go-git + chroma.
- TS: `@pierre/diffs` + Shiki = what hunk ships (adopting their model wholesale).

Avoid: per-hunk isolated highlighting; Myers default; difftastic as primary engine.

Sources: delta github.com/dandavison/delta · pierre.computer/writing/on-rendering-diffs ·
git-scm.com/docs/diff-options · github.com/Wilfred/difftastic/issues/153 · git contrib/diff-highlight
