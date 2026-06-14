# Per-source review state

Review state (comments + viewed marks) is currently a single `Session` keyed
only by file path and persisted at `.diffler/session.json` — implicitly "the
working tree". Opening a commit or range diff reuses that one session, so:

- marking a file viewed / commenting on a commit or range diff looks the file up
  in the working-tree model and fails with `"<path> is not part of the review
  diff"` (the file lives in the pinned `commit_model`, not `review.model()`);
- even when it resolves, the mark/comment would be stored under the working-tree
  session keyed by bare path, colliding with the working-tree review;
- the agent (MCP) only ever sees the working-tree session, with no idea which
  diff the human reviewed or where those changes came from.

Goal: review state attaches to a **source** (working tree / a commit / a commit
range), is persisted deterministically per source, and every comment the agent
sees carries its source provenance.

Decisions (locked with the user): full rework now; one file per source on disk;
MCP aggregates all reviews tagged with their source.

## Core: `ReviewSource` (diffler-core)

New `source.rs`:

```rust
pub enum ReviewSource {
    WorkingTree,
    Commit(String),                 // full oid
    Range { oldest: String, newest: String },  // full oids
}
```

- `key(&self) -> String` — deterministic, filesystem-safe: `working`,
  `commit-<oid>`, `range-<oldest>-<newest>`. This is the persistence key.
- `label(&self) -> String` — human: `working tree`, `commit <short>`,
  `range <short>..<short>` (the TUI/agent-facing description).
- `Serialize`/`Deserialize` as a tagged descriptor so each on-disk file is
  self-describing.

The bin's existing `DiffSource` (`app/diff.rs`, same three variants) is replaced
by this one type — single source of truth, no parallel enum.

## Core: per-source persistence (store.rs)

Layout: `.diffler/reviews/<key>.json`. Each file:

```jsonc
{ "version": 1, "source": { "kind": "commit", "oid": "…" }, /* flattened Session */ }
```

- `load_source(repo_root, &ReviewSource) -> Session`. For `WorkingTree`, if
  `reviews/working.json` is missing, fall back to the legacy
  `.diffler/session.json` (migration-on-read); the next `save_source` writes the
  new path and the legacy file is removed once migrated.
- `save_source(repo_root, &ReviewSource, &Session)` — atomic write (temp +
  rename) into `reviews/`, same `.gitignore` guard as today.
- `load_all(repo_root) -> Vec<(ReviewSource, Session)>` — reads every
  `reviews/*.json` (plus legacy working) for MCP aggregation. Surfaces a
  corrupt file as `StoreError::Corrupt` rather than silently dropping it.
- Keep the old `load`/`save` as thin wrappers over the working source so nothing
  breaks mid-refactor.

## Core: `Review` facade

`Review` keeps `session` (the working-tree session, unchanged for the ~30
working-tree call sites) and adds a lazily-loaded cache for other sources:

- `sources: HashMap<String /* key */, Session>`.
- `ensure_source(&mut self, &ReviewSource) -> Result<(), ReviewError>` — loads a
  non-working source into the cache if absent.
- `session_for(&self, &ReviewSource) -> &Session` / `session_for_mut(&mut self,
  &ReviewSource) -> &mut Session` — `WorkingTree` maps to `session`; others to
  the cache (caller must `ensure_source` first; mut uses `entry().or_default()`).
- `save_for(&self, &ReviewSource) -> Result<(), ReviewError>`.
- `all_reviews(&self) -> Vec<(ReviewSource, &Session)>` — in-memory working +
  cached sources, merged over `store::load_all` (in-memory wins) for MCP.

## Bin: diff view (app/diff.rs, ui/diff.rs)

- Replace `DiffSource` with `ReviewSource`.
- `DiffView::model`, `ensure_rows`/`build_rows`, comment add/resolve, and
  `diff_toggle_viewed` use `review.session_for(&self.source)` /
  `session_for_mut`, and look the file up in `self.diff.model(review)` (the
  pinned model for commit/range), not `review.model()`. This is the direct fix
  for `"<path> is not part of the review diff"`.
- On open of a commit/range view, `review.ensure_source(&source)` then
  `save_for` on every mutation (mirrors the working-tree `review.save()`).

## Bin: MCP (app/mcp.rs, protocol types)

- `CommentInfo` and the per-file `ReviewStatus` entries gain a `source` field
  (key + label + endpoints).
- `GetComments` / `Feedback` / `review_status` aggregate over `all_reviews`,
  each comment tagged with its source — the agent always sees what the human
  reviewed and where it came from.
- New `ListReviews` request: enumerate sources with open/total counts.
- `ReplyComment` / `ProposeResolve` / `MarkViewed` resolve the comment's owning
  source (look up which review holds the id) and act + persist there.

## Tests / gate

- core: `ReviewSource::key`/`label`; store round-trip per source; legacy
  migration (old `session.json` → `working.json`, legacy removed); `load_all`.
- bin: viewed + comment on a commit diff and a range diff persist under that
  source and do NOT touch the working-tree session; the `"not part of"` error is
  gone; MCP aggregation tags comments with source; agent reply/resolve targets
  the right source.
- e2e: open a commit diff, mark viewed, reopen → still viewed; working tree
  unaffected.
- `just ci` + `just e2e` green at each stage. Snapshots: comment rendering in a
  commit diff (new). Read `.snap.new`.

## Out of scope

Comments "following" content from the working tree into a commit when you
commit; cross-source dedup; pruning old review files (manual rm of
`.diffler/reviews/*` for now).
