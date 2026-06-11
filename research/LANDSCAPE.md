# Competitive Landscape — 2026-06-11

Prior research (June 2) covered tuicr, difftastic, delta, lazygit, gitui — and missed the entire
2025–2026 "review-the-agent" category. Sweep results, ordered by relevance.

## Direct competitors

### hunk (modem-dev) — closest competitor
- github.com/modem-dev/hunk · MIT · 4.7k★ · v0.15.1 (Jun 2026), very active
- "Review-first terminal diff viewer for agentic coders." Full-screen TUI, watch mode (live updates),
  live session system: TUI registers with local loopback daemon; agent posts inline comments into the
  live UI via `hunk session` CLI + a Claude skill.
- Stack: **TypeScript on OpenTUI** + Pierre diffs. Heavy, anti-suckless.
- Gaps vs diffler: bespoke daemon+skill, **not MCP**; no hunk accept/reject; no workload tracking.
- Borrow: live-session registration (agent finds the right live window); `--agent-context` workflow.

### revdiff (umputun)
- github.com/umputun/revdiff · MIT · Go · 541★ · v1.6.1 (Jun 7 2026)
- Inline annotations on diffs/plans/docs; annotations → structured stdout + exit code 10 so agents
  loop review→fix→re-review. Claude Code plugin, Codex/OpenCode/Pi integrations. git/hg/jj.
- Gaps: one-shot exit-code model — no live updates, no MCP, no accept/reject, no tracking.
- Borrow: structured annotation output format; terminal-overlay detection (tmux/zellij/kitty/wezterm).

### crit (tomasz-tomczyk)
- github.com/tomasz-tomczyk/crit · MIT · Go + TS web UI · 483★ · v0.16.2 (Jun 2026)
- Local **web** UI: review plans/diffs, inline comments, **round-to-round diffs between agent
  iterations**, loop until approval. Explicitly no hunk accept/reject. No MCP.
- Borrow: round-to-round iteration diffing; "unresolved comments block approval" semantics.

### vibe-kanban (BloopAI) — market validation, now vacating
- github.com/BloopAI/vibe-kanban · Apache-2.0 · 26.9k★ · **SUNSETTING** (going community-maintained)
- Kanban orchestrating 10+ agents, worktree per task, card → Review → line-by-line diff with inline
  comments back to agent, reject → new Attempt. Web app (Rust + TS).
- Verdict: strongest proof "review + workload tracking for agents" is real; its sunset displaces
  users. Diffler = the terminal-native answer. Borrow: card lifecycle, attempt/retry semantics.

### sidecar + td (haplab)
- github.com/marcus/sidecar (Go TUI, MIT, 1k★) — live-updating diff dashboard beside CLI agents;
  passive observer: no comments, no verdicts, no MCP.
- td (td.haplab.com) — Go+SQLite agent task backlog, kanban TUI, enforced cross-session review.
- Gap they leave: neither couples tracking with interactive diff review. Diffler fuses both via MCP.

## Second tier

| Tool | What | Verdict |
|---|---|---|
| difit (yoshiko-pg) | Local web GitHub-style diff; comments export as agent prompts; big in CC skill ecosystem | web, no MCP/verdicts |
| diffx (wong2) | Local web PR review; comment status open/replied/resolved, XML export | borrow comment-status model |
| prr | GitHub PRs as text files in $EDITOR | inspiration: file-as-review-interface |
| scm-record (jj) | Interactive hunk selection TUI | **best-in-class terminal hunk-picker UX** — study it |
| jjui / lazyjj | jj TUIs | watch jj ecosystem |
| md-redline | Markdown review comments with **built-in MCP server** | proves MCP-review-surface pattern, tiny scope |
| intellij-local-review | IDE inline comments → agents via MCP toolset | same pattern, IDE |
| kagan / openkanban / kanban-md / operator / tmuxcc | kanban-for-agents TUIs | none do diff review |
| diffity / Codiff / Stage / Deff / diffai / gh-dash / gh-pr-review | viewers or GitHub-remote | irrelevant |

## Platform pressure

- **Claude Code**: per-edit y/n/d/e accept exists; **hunk-level review is a top-requested missing
  feature** (issues #31395, #42448, #44787). Anthropic may ship it — speed matters.
- **Codex CLI**: inline review comments, plan-step approve/reject, Guardian approval subagent.
- Defense: agent-agnostic (standard MCP), persistent surface, terminal-native, suckless binary.

## Gaps diffler fills (nobody does all)

1. **MCP as the review protocol in a terminal TUI** — zero tools found. hunk = custom daemon,
   revdiff = exit codes, crit/difit = slash commands, md-redline = MCP but markdown-only.
   Standard MCP ⇒ every harness works, zero per-agent plugins.
2. **Hunk-level accept/reject in an agent loop** — absent from hunk, revdiff, crit, diffx;
   top-requested CC feature. Only scm-record has the UX, with no agent loop.
3. **Review + workload tracking in one surface** — disjoint ecosystems today; vibe-kanban had both
   (web) and is sunsetting.
4. **Live bidirectional terminal surface** — only hunk and sidecar live-update; neither has verdicts
   or MCP.
5. **Suckless positioning** — competitors are TS/OpenTUI, web UIs, or Rust+React web. Single small
   binary, no browser, no daemon sprawl: unoccupied.

## Watch list

hunk (could add MCP any release) · revdiff (umputun ships fast) · Claude Code / Codex builtins ·
vibe-kanban community fork direction.
