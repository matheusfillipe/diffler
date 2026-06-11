# Workspace Isolation — Worktrees and Alternatives (2026-06-11)

User instinct ("worktrees are terrible") is well-supported. Survey of what agent tools use and what
diffler should do.

## Why worktrees hurt

Every major orchestrator converged on worktrees (vibe-kanban, claude-squad, Crystal, Conductor,
Claude Code EnterWorktree) and they all share the same wounds — worktrees only isolate *tracked*
files:
- deps not installed per worktree (node_modules/venv); none of the tools auto-install
- untracked config vanishes (.env, .envrc, secrets)
- disk bloat (reported 9.82 GB over a 2 GB codebase in 20 min)
- cold build caches, port conflicts, shared-dev-DB collisions, Docker project-name collisions
- IDE support late (VS Code Jul 2025, JetBrains 2026.1)

Mitigations exist (pnpm store, APFS `cp -c`, workz symlink/env-copy wrapper) — all patches on a leaky
model.

## Alternatives

- **jj workspaces**: shared commit graph, auto-snapshotting working copy (agent work continuously
  captured). Ergonomics win is the snapshotting, not the workspace (still separate dirs, same dep
  problem). Agent-tool adoption early/enthusiast (jj-navi, jj-guide skill). Colocated jj repo = normal
  git repo on disk ⇒ plain git diff works against it.
- **Containers per agent**: dagger/container-use (MCP server, fresh container + branch per agent),
  Imbue Sculptor (devcontainer image bakes deps once). The real isolation answer — orchestrator
  territory.
- **GitButler virtual branches**: N branches applied to ONE working dir; changes *assigned* to
  branches; agents drive via `but` CLI. Trigger.dev publicly ditched worktrees for it. Caveat: no
  isolation of concurrent *execution* (two agents racing in one dir) — isolates commits, not processes.
- **Branch-switching single checkout**: not viable with concurrent agents (checkout mutates shared
  files). Nobody does this.
- **"Review without isolation"**: agent commits to branch, human reviews committed branch/range. No
  parallel working tree needed. revdiff (`/revdiff main`, last N commits) and tuicr (PR-style) do this.

## Decision for diffler

**Isolation is the orchestrator's problem. Diffler reads what the agent produced — model-agnostic.**

v1 diff sources, priority order:
1. **Working tree** (staged + unstaged + untracked) — "what did the agent just do". Default of hunk,
   revdiff, tuicr, crit.
2. **Arbitrary ref/range**: `diffler main`, `diffler HEAD~3`, `diffler a..b`, `diffler <commit>` —
   covers the commit-to-branch review model, sidesteps the worktree debate entirely.
3. **Patch file / two files** (hunk and Pierre support this).

v1.5 optional: `git worktree list` enumeration → pick which worktree's tree to diff (cheap, useful
when launched outside the orchestrator's dir). Do NOT build worktree create/manage. Do NOT manage
containers/virtual branches/jj — just diff correctly against arbitrary refs (jj colocated works free).

The "Workspaces" section of the home screen = working tree + branches + (if present) existing
worktrees as *read-only diff sources*, not managed objects.

Sources: medium.com/@rohansx (worktree pain survey) · trigger.dev/blog/parallel-agents-gitbutler ·
github.com/dagger/container-use · imbue.com/blog/sculptor-announce · docs.gitbutler.com ·
panozzaj.com jj-for-agents · revdiff.com/docs · tuicr.dev
