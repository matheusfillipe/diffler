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

Run Claude from the repo root and the proxy auto-discovers the port from
`.diffler/mcp.json`. No diffler running ⇒ the proxy exits with a clear error.

## Configuration

Resolution order (first match wins):

1. `--url <url>` / `DIFFLER_MCP_URL` — full endpoint, e.g. `http://127.0.0.1:8417/mcp`
2. `--port <n>` / `DIFFLER_MCP_PORT` and `--host <h>` / `DIFFLER_MCP_HOST`
3. the live port in `<repo>/.diffler/mcp.json` (`--repo <path>`, default: cwd)
4. `http://127.0.0.1:8417/mcp`

```json
{
  "mcpServers": {
    "diffler": {
      "command": "npx",
      "args": ["-y", "diffler-mcp", "--port", "8417"],
      "env": { "DIFFLER_MCP_HOST": "127.0.0.1" }
    }
  }
}
```

## Prefer HTTP directly?

Claude Code speaks HTTP natively, so you can skip this proxy entirely:

```bash
claude mcp add --transport http diffler http://127.0.0.1:8417/mcp
```

The proxy exists for the `npx`, zero-config, auto-port-discovery ergonomics.
