#!/usr/bin/env node
// stdio↔HTTP MCP proxy: Claude Code spawns this over stdio, and it forwards
// every tool call to the streamable-HTTP MCP server embedded in a running
// diffler TUI. diffler's MCP serves the live review state, so the proxy never
// owns state — it only bridges transports.

import { readFileSync } from "node:fs";
import { join } from "node:path";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";

const DEFAULT_HOST = "127.0.0.1";
const DEFAULT_PORT = 8417;

function parseArgs(argv) {
  const opts = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const take = () => argv[(i += 1)];
    switch (arg) {
      case "--url":
        opts.url = take();
        break;
      case "--host":
        opts.host = take();
        break;
      case "--port":
        opts.port = take();
        break;
      case "--repo":
        opts.repo = take();
        break;
      default:
        // unknown flags are ignored so future diffler args don't break older proxies
        break;
    }
  }
  return opts;
}

// The port may differ from the configured one after an ephemeral fallback, so
// diffler publishes the live endpoint to .diffler/mcp.json in the repo.
function discoverPort(repo) {
  try {
    const raw = readFileSync(join(repo, ".diffler", "mcp.json"), "utf8");
    const port = JSON.parse(raw).port;
    return typeof port === "number" ? port : undefined;
  } catch {
    return undefined;
  }
}

function resolveUrl() {
  const opts = parseArgs(process.argv.slice(2));
  const env = process.env;
  const explicit = opts.url || env.DIFFLER_MCP_URL;
  if (explicit) {
    return explicit;
  }
  const host = opts.host || env.DIFFLER_MCP_HOST || DEFAULT_HOST;
  const repo = opts.repo || process.cwd();
  const port =
    opts.port || env.DIFFLER_MCP_PORT || discoverPort(repo) || DEFAULT_PORT;
  return `http://${host}:${port}/mcp`;
}

async function main() {
  const url = resolveUrl();

  const client = new Client({ name: "diffler-mcp-proxy", version: "0.1.0" });
  try {
    await client.connect(new StreamableHTTPClientTransport(new URL(url)));
  } catch (err) {
    process.stderr.write(
      `diffler-mcp: cannot reach a diffler MCP server at ${url}\n` +
        `Is diffler running in this repo? (${err.message ?? err})\n`,
    );
    process.exit(1);
  }

  const server = new Server(
    { name: "diffler", version: "0.1.0" },
    { capabilities: { tools: {} } },
  );
  server.setRequestHandler(ListToolsRequestSchema, () => client.listTools());
  server.setRequestHandler(CallToolRequestSchema, (request) =>
    client.callTool(request.params),
  );

  // when diffler exits the HTTP side drops; tear down so Claude sees the close
  client.onclose = () => {
    void server.close().finally(() => process.exit(0));
  };

  await server.connect(new StdioServerTransport());
}

main().catch((err) => {
  process.stderr.write(`diffler-mcp: ${err.stack ?? err}\n`);
  process.exit(1);
});
