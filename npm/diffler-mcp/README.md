# diffler-mcp

A tiny stdio↔HTTP bridge that lets Claude Code (or any stdio MCP client) talk to
the MCP server embedded in a running [diffler](https://github.com/matheusfillipe/diffler)
review session.

diffler's MCP server runs **inside the TUI** as a streamable-HTTP endpoint
(`http://127.0.0.1:8417/mcp` by default) because it serves the live review state
on the app's main loop. This proxy is spawned by Claude over stdio and forwards
every tool call to that endpoint — it owns no state itself.

## Use it with Claude Code

Run diffler in your repo (it prints the connect hint and writes
`.diffler/mcp.json` with the live port), then:

```bash
claude mcp add diffler -- npx -y diffler-mcp
```

Or in a checked-in `.mcp.json`:

```json
{
  "mcpServers": {
    "diffler": {
      "command": "npx",
      "args": ["-y", "diffler-mcp"]
    }
  }
}
```

Start Claude anywhere inside the repo and the proxy auto-discovers the port
from `.diffler/mcp.json`. No diffler running ⇒ every tool call reports which
directory it searched.

## Configuration

Resolution order (first match wins):

1. `--url <url>` / `DIFFLER_MCP_URL` — full endpoint, e.g. `http://127.0.0.1:8417/mcp`
2. `--port <n>` / `DIFFLER_MCP_PORT` and `--host <h>` / `DIFFLER_MCP_HOST`
3. the live port in the nearest `.diffler/mcp.json`, searching the start
   directory (`--repo <path>`, default: cwd) and then each parent

Discovery covers the normal case, so nothing needs configuring:

```json
{
  "mcpServers": {
    "diffler": {
      "command": "npx",
      "args": ["-y", "diffler-mcp"]
    }
  }
}
```

Reach for `--port`, `--host` or `--repo` when diffler runs somewhere the walk-up
cannot see, such as another machine over a tunnel.

## Prefer HTTP directly?

Claude Code speaks HTTP natively, so you can skip this proxy entirely:

```bash
claude mcp add --transport http diffler http://127.0.0.1:8417/mcp
```

The proxy exists for the `npx`, zero-config, auto-port-discovery ergonomics.
