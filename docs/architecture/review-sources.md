# Review sources

## Review state is per diff source

A `ReviewSource` is `WorkingTree`,
`Commit{oid}`, `Range{oldest,newest}`, `Pr{number}`, `Against{rev}`, or
`Walkthrough{id}`. Comments (anchored to file + line +
a `line_text` snapshot so stale anchors show as outdated; visual mode anchors a
range; status Open/Replied/Resolved + threads) and GitHub-style viewed marks
(keyed by file content hash, auto-cleared on change) are stored **per source**:
`.diffler/reviews/<key>.json` where key ∈ {`working`, `commit-<oid>`,
`range-<a>-<b>`, `pr-<n>`, `against-<rev>`, `walkthrough-<id>`}. Legacy
`.diffler/session.json` migrates to `reviews/working.json`; a review file
written before a walkthrough was a source of its own, carrying it embedded,
splits it into its own `walkthrough-<id>.json` on load (see
[walkthrough.md](walkthrough.md)).
`.diffler/` self-gitignores. No daemon: agent tool calls fail while the TUI is
down (by design, harnesses retry).

## Three-dot review (`Against{rev}`)

`d` on the status screen opens the diff
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
