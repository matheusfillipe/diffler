#!/usr/bin/env node
// stdio↔HTTP MCP proxy bridging Claude Code to a running diffler TUI, lazily
// (re)connecting so it survives diffler quitting and restarting on a new port.

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
    const take = () => argv[(i += 1)];
    switch (argv[i]) {
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
        break;
    }
  }
  return opts;
}

function discoverPort(repo) {
  try {
    const port = JSON.parse(readFileSync(join(repo, ".diffler", "mcp.json"), "utf8")).port;
    return typeof port === "number" ? port : undefined;
  } catch {
    return undefined;
  }
}

function resolveUrl(opts) {
  const env = process.env;
  const explicit = opts.url || env.DIFFLER_MCP_URL;
  if (explicit) {
    return explicit;
  }
  const host = opts.host || env.DIFFLER_MCP_HOST || DEFAULT_HOST;
  const repo = opts.repo || process.cwd();
  const port = opts.port || env.DIFFLER_MCP_PORT || discoverPort(repo) || DEFAULT_PORT;
  return `http://${host}:${port}/mcp`;
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const server = new Server(
    { name: "diffler", version: "0.1.0" },
    { capabilities: { tools: {} } },
  );

  let upstream = null;
  let connecting = null;

  const connect = async () => {
    const client = new Client({ name: "diffler-mcp-proxy", version: "0.1.0" });
    await client.connect(new StreamableHTTPClientTransport(new URL(resolveUrl(opts))));
    // cache synchronously after the await so a later close can't race ahead of it
    upstream = client;
    const drop = () => {
      if (upstream === client) {
        upstream = null;
      }
    };
    client.onclose = drop;
    client.onerror = drop;
    server.sendToolListChanged?.().catch(() => {});
    return client;
  };

  const ensureUpstream = () => {
    if (upstream) {
      return Promise.resolve(upstream);
    }
    if (!connecting) {
      connecting = connect().finally(() => {
        connecting = null;
      });
    }
    return connecting;
  };

  const withUpstream = async (fn) => {
    try {
      return await fn(await ensureUpstream());
    } catch {
      upstream = null;
      return await fn(await ensureUpstream());
    }
  };

  server.setRequestHandler(ListToolsRequestSchema, async () => {
    try {
      return await withUpstream((client) => client.listTools());
    } catch {
      return { tools: [] };
    }
  });
  server.setRequestHandler(CallToolRequestSchema, async (request) => {
    try {
      return await withUpstream((client) => client.callTool(request.params));
    } catch (err) {
      upstream = null;
      return {
        isError: true,
        content: [
          {
            type: "text",
            text: `diffler isn't reachable — is it running in this repo? (${err.message ?? err})`,
          },
        ],
      };
    }
  });

  await server.connect(new StdioServerTransport());
  void ensureUpstream().catch(() => {});
}

main().catch((err) => {
  process.stderr.write(`diffler-mcp: ${err.stack ?? err}\n`);
  process.exit(1);
});
