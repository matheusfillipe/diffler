# diffler-mcp

A stdio↔HTTP bridge from Claude Code (or any stdio MCP client) to the MCP
server of a running [diffler](https://github.com/matheusfillipe/diffler).

diffler serves MCP from inside the TUI as a streamable-HTTP endpoint
(`http://127.0.0.1:8417/mcp` by default). The proxy forwards every tool call
to that endpoint and keeps no state of its own.

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

Start Claude anywhere inside the repo and the proxy finds the port in
`.diffler/mcp.json`. With no diffler running, every tool call reports which
directory it searched.

## Configuration

Resolution order (first match wins):

1. `use_instance` called earlier in this session
2. `--url <url>` / `DIFFLER_MCP_URL`: full endpoint, e.g. `http://127.0.0.1:8417/mcp`
3. `--port <n>` / `DIFFLER_MCP_PORT` and `--host <h>` / `DIFFLER_MCP_HOST`
4. the live port in the nearest `.diffler/mcp.json`, searching the start
   directory (`--repo <path>`, default: cwd) and then each parent, if that
   diffler is still running
5. the per-user instance registry (`$XDG_STATE_HOME/diffler/instances`,
   `~/.local/state` by default): the most recently started live diffler;
   `use_instance` switches; several are listed by `list_instances`

`use_instance` connects before it answers, so a bad target never gets bound.
When diffler isn't running yet, the proxy keeps retrying and announces its
tools once one appears, so you can start Claude before diffler. A call diffler
doesn't answer within 110 s fails with that instance named.

Use `--port`, `--host` or `--repo` when diffler runs somewhere the walk-up
cannot see, such as another machine over a tunnel.

A human's diffler and an agent's shell are often in different repos. The proxy
adds two tools for that, alongside diffler's own:

- **list_instances**: every running diffler this proxy can reach, across all
  repos, from the registry.
- **use_instance**: point this proxy at one of them, by repo path (or a
  unique directory-name suffix) or port, for the rest of this session.

## Prefer HTTP directly?

Claude Code speaks HTTP, so you can skip the proxy and use a fixed port:

```bash
claude mcp add --transport http diffler http://127.0.0.1:8417/mcp
```
