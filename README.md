# diffler

> Work in progress. Nothing usable here yet.

Terminal code review for AI coding agents.

A TUI you launch in a repo alongside Claude Code or any MCP-compatible agent.
It shows a live-updating diff of what the agent is doing; you leave comments and
accept or reject hunks; the agent reads your feedback over MCP and responds.
Review and workload tracking in one terminal surface — no browser, no daemon,
one binary.

## Why

Editors are optimized for writing code. Agents write the code now. Reviewing
their output in the terminal is the missing piece.

## Status

Pre-alpha scaffold. See `PLAN.md`.

## Development

```sh
just ci    # fmt + clippy + tests, same gate as CI
```

Requires Rust 1.88+, `just`, `cargo-nextest`. Hooks: `prek install`.

## License

MIT or Apache-2.0, at your option.
