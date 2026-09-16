#!/usr/bin/env node
// stdio↔HTTP MCP proxy bridging Claude Code to a running diffler TUI, lazily
// (re)connecting so it survives diffler quitting and restarting on a new port.

import { readFileSync, readdirSync, statSync, unlinkSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";

const DEFAULT_HOST = "127.0.0.1";
const ENDPOINT_FILE = join(".diffler", "mcp.json");
// A missing diffler is common (a human starts Claude before the TUI), so the
// background retry below stops at a ceiling.
const RETRY_MS = Number(process.env.DIFFLER_MCP_RETRY_MS) || 3000;
const RETRY_MAX_MS = Number(process.env.DIFFLER_MCP_RETRY_MAX_MS) || 10 * 60 * 1000;
// Longer than a client's own request ceiling, so a hung diffler fails the
// call itself, with the diagnosis this proxy can give.
const CALL_DEADLINE_MS = Number(process.env.DIFFLER_MCP_CALL_DEADLINE_MS) || 110_000;

const PROXY_TOOLS = [
  {
    name: "list_instances",
    description: "List every running diffler instance this proxy can reach, across all repos.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
  },
  {
    name: "use_instance",
    description:
      "Point this proxy at one diffler instance, by repo path (or a unique directory-name suffix) or port.",
    inputSchema: {
      type: "object",
      properties: {
        repo: { type: "string" },
        port: { type: "number" },
      },
      additionalProperties: false,
    },
  },
];

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

// A process that no longer exists (ESRCH) is dead; anything else (alive, or
// EPERM because it's owned by someone else) counts as alive. A missing pid
// means an older TUI that predates this field, which we can't disprove.
function pidAlive(pid) {
  if (pid == null) {
    return true;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return err.code !== "ESRCH";
  }
}

// Each diffler publishes its live port under its own repo root, so the walk-up
// finds the instance owning the directory the editor launched from. Returns
// null both when nothing is found and when the closest file names a dead
// process, so the caller falls through to the cross-repo registry either way.
function findLocalEndpoint(from) {
  let dir = resolve(from);
  for (;;) {
    try {
      const { port, pid } = JSON.parse(readFileSync(join(dir, ENDPOINT_FILE), "utf8"));
      if (typeof port === "number") {
        return pidAlive(pid) ? port : null;
      }
    } catch {
      // unreadable or malformed reads like absent: keep walking up
    }
    const parent = dirname(dir);
    if (parent === dir) {
      return null;
    }
    dir = parent;
  }
}

// Mirrors how the diffler binary resolves XDG_STATE_HOME, so both sides agree
// on where the per-user instance registry lives.
function registryDir() {
  const base = process.env.DIFFLER_STATE_DIR || process.env.XDG_STATE_HOME || join(homedir(), ".local", "state");
  return join(base, "diffler", "instances");
}

// Every running diffler that isn't the one found by the walk-up: read from
// the registry, dropping (and deleting) entries whose process has exited.
function liveRegistry() {
  let files;
  try {
    files = readdirSync(registryDir());
  } catch {
    return [];
  }
  const live = [];
  for (const file of files) {
    if (!file.endsWith(".json")) {
      continue;
    }
    const path = join(registryDir(), file);
    let entry;
    try {
      entry = JSON.parse(readFileSync(path, "utf8"));
    } catch {
      continue;
    }
    if (!pidAlive(entry.pid)) {
      try {
        unlinkSync(path);
      } catch {
        // another proxy already pruned it
      }
      continue;
    }
    entry.mtimeMs = statSync(path).mtimeMs;
    live.push(entry);
  }
  return live;
}

function describeInstances(instances) {
  return instances.map((i) => `${i.repo} (port ${i.port})`).join(", ");
}

function matchInstances(instances, { repo, port }) {
  if (port != null) {
    return instances.filter((i) => i.port === Number(port));
  }
  if (!repo) {
    return [];
  }
  const exact = instances.filter((i) => i.repo === repo);
  if (exact.length) {
    return exact;
  }
  return instances.filter((i) => i.repo.endsWith(`/${repo}`) || i.repo.endsWith(`\\${repo}`));
}

// Best-effort label for an error message: which repo (and port) a URL names,
// falling back to the port alone when the registry doesn't know it.
function describeUrl(url) {
  const match = liveRegistry().find((i) => i.url === url);
  if (match) {
    return `${match.repo} (port ${match.port})`;
  }
  try {
    return `port ${new URL(url).port}`;
  } catch {
    return "diffler";
  }
}

class CallDeadline extends Error {}

// Dropping the `upstream` reference alone leaves the transport's
// AbortController and reconnection timer alive, so every place that
// abandons a client closes it first.
function closeQuietly(client) {
  if (!client) {
    return;
  }
  client.close().catch(() => {});
}

// A call to the upstream that never answers must not hang the client
// forever; racing it against a timer is what lets the proxy give up and
// report it. `fn` is what to race (a tool call, a tools/list), so both share
// the same deadline and reconnect handling in `withUpstream` below.
function withDeadline(client, fn, meta) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new CallDeadline(meta)), CALL_DEADLINE_MS);
  });
  return Promise.race([fn(client), timeout]).finally(() => clearTimeout(timer));
}

function toolError(text) {
  return { isError: true, content: [{ type: "text", text }] };
}

function deadlineError(meta) {
  const seconds = Math.round(CALL_DEADLINE_MS / 1000);
  return toolError(`${meta || "diffler"} did not answer within ${seconds}s. Call again to reconnect.`);
}

function unreachableError(err) {
  const instances = liveRegistry();
  const extra = instances.length
    ? ` Other diffler instances running: ${describeInstances(instances)}.`
    : "";
  return toolError(`diffler isn't reachable. Is it running in this repo? (${err.message ?? err})${extra}`);
}

// Set by the use_instance tool; overrides every other resolution source for
// the rest of this proxy process.
let overrideUrl = null;

// `ambiguous` marks the one resolution path with no explicit say-so from the
// human or the caller: more than one diffler is running and the newest was
// picked for them. `forwardCall` uses it to tell the agent once per bind.
function resolveUrl(opts) {
  if (overrideUrl) {
    return { url: overrideUrl, ambiguous: false };
  }
  const env = process.env;
  const explicit = opts.url || env.DIFFLER_MCP_URL;
  if (explicit) {
    return { url: explicit, ambiguous: false };
  }
  const host = opts.host || env.DIFFLER_MCP_HOST || DEFAULT_HOST;
  const explicitPort = opts.port || env.DIFFLER_MCP_PORT;
  if (explicitPort) {
    return { url: `http://${host}:${explicitPort}/mcp`, ambiguous: false };
  }
  const local = findLocalEndpoint(opts.repo || process.cwd());
  if (local !== null) {
    return { url: `http://${host}:${local}/mcp`, ambiguous: false };
  }
  const instances = liveRegistry();
  if (instances.length === 1) {
    return { url: instances[0].url, ambiguous: false };
  }
  if (instances.length > 1) {
    const newest = instances.reduce((a, b) => (b.mtimeMs > a.mtimeMs ? b : a));
    process.stderr.write(
      `diffler-mcp: several diffler instances are running (${describeInstances(instances)}); binding the most recently started, ${newest.repo} (port ${newest.port}). Call use_instance to switch.\n`,
    );
    return { url: newest.url, ambiguous: true };
  }
  throw new Error(
    `no diffler is running in ${resolve(opts.repo || process.cwd())} or any parent (no ${ENDPOINT_FILE})`,
  );
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const server = new Server(
    { name: "diffler", version: "0.1.0" },
    { capabilities: { tools: {} } },
  );

  let upstream = null;
  let upstreamMeta = null;
  let connecting = null;
  let retryActive = false;
  // Set on an ambiguous auto-bind (several instances, newest picked with no
  // say-so); `forwardCall` reports it on the first result after and clears
  // it. A resolution the caller or the human chose leaves this null.
  let pendingBindNotice = null;

  const connect = async () => {
    const { url, ambiguous } = resolveUrl(opts);
    const client = new Client({ name: "diffler-mcp-proxy", version: "0.1.0" });
    await client.connect(new StreamableHTTPClientTransport(new URL(url)));
    // One round trip before caching: a registered pid can be alive while the
    // MCP server behind it is not (mid-restart, a crashed listener), and this
    // is what tells the two apart.
    await client.listTools();
    // cache synchronously after the await so a later close can't race ahead of it
    upstream = client;
    upstreamMeta = describeUrl(url);
    pendingBindNotice = ambiguous
      ? `Bound to ${upstreamMeta} because several diffler instances are running; call use_instance to switch.`
      : null;
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

  // Runs `fn` against the live upstream client under the deadline above; a
  // deadline drops the upstream and fails outright, no second wait on the
  // same call, while any other failure gets one reconnect-and-retry. Shared
  // by tools/list and tools/call, so a hung diffler fails either one alike.
  const withUpstream = async (fn) => {
    let client;
    const attempt = async () => {
      client = await ensureUpstream();
      const meta = upstreamMeta;
      try {
        return await withDeadline(client, fn, meta);
      } catch (err) {
        if (err instanceof CallDeadline) {
          closeQuietly(client);
          upstream = null;
        }
        throw err;
      }
    };
    try {
      return await attempt();
    } catch (err) {
      if (err instanceof CallDeadline) {
        throw err;
      }
      closeQuietly(client);
      upstream = null;
      return await attempt();
    }
  };

  // Lets a human start Claude first and diffler second: a `tools/list` that
  // finds nothing arms this, and it keeps trying quietly until diffler
  // appears (or ten minutes pass), announcing the new tools once it does.
  const startRetryingUpstream = () => {
    if (retryActive) {
      return;
    }
    retryActive = true;
    const deadline = Date.now() + RETRY_MAX_MS;
    const attempt = async () => {
      try {
        await ensureUpstream();
        retryActive = false;
        server.sendToolListChanged?.().catch(() => {});
        return;
      } catch {
        // still unreachable
      }
      if (Date.now() >= deadline) {
        retryActive = false;
        return;
      }
      setTimeout(attempt, RETRY_MS);
    };
    setTimeout(attempt, RETRY_MS);
  };

  // Forwards one tool call, appending the ambiguous-bind notice to the first
  // result after such a bind (once only: it clears itself here).
  const forwardCall = async (params) => {
    try {
      const result = await withUpstream((client) => client.callTool(params));
      if (!pendingBindNotice) {
        return result;
      }
      const notice = pendingBindNotice;
      pendingBindNotice = null;
      return { ...result, content: [...(result.content ?? []), { type: "text", text: notice }] };
    } catch (err) {
      return err instanceof CallDeadline ? deadlineError(err.message) : unreachableError(err);
    }
  };

  const currentUrl = () => {
    try {
      return resolveUrl(opts).url;
    } catch {
      return null;
    }
  };

  const handleListInstances = () => {
    const here = currentUrl();
    const instances = liveRegistry().map((i) => ({
      repo: i.repo,
      port: i.port,
      pid: i.pid,
      current: i.url === here,
    }));
    return { content: [{ type: "text", text: JSON.stringify({ instances }) }] };
  };

  const handleUseInstance = async (args) => {
    const instances = liveRegistry();
    const matches = matchInstances(instances, args);
    if (matches.length === 0) {
      return toolError(
        `no running instance matches ${JSON.stringify(args)}. Choices: ${describeInstances(instances) || "none running"}.`,
      );
    }
    if (matches.length > 1) {
      return toolError(
        `${JSON.stringify(args)} matches more than one instance: ${describeInstances(matches)}. Be more specific.`,
      );
    }
    const target = matches[0];
    const previousOverride = overrideUrl;
    overrideUrl = target.url;
    closeQuietly(upstream);
    upstream = null;
    try {
      await ensureUpstream();
    } catch (err) {
      overrideUrl = previousOverride;
      return toolError(`could not connect to ${target.repo} (port ${target.port}): ${err.message ?? err}`);
    }
    server.sendToolListChanged?.().catch(() => {});
    return {
      content: [
        { type: "text", text: JSON.stringify({ ok: true, repo: target.repo, port: target.port }) },
      ],
    };
  };

  server.setRequestHandler(ListToolsRequestSchema, async () => {
    let upstreamTools = [];
    try {
      upstreamTools = (await withUpstream((client) => client.listTools())).tools ?? [];
    } catch {
      upstreamTools = [];
      startRetryingUpstream();
    }
    return { tools: [...PROXY_TOOLS, ...upstreamTools] };
  });
  server.setRequestHandler(CallToolRequestSchema, async (request) => {
    if (request.params.name === "list_instances") {
      return handleListInstances();
    }
    if (request.params.name === "use_instance") {
      return handleUseInstance(request.params.arguments ?? {});
    }
    return forwardCall(request.params);
  });

  await server.connect(new StdioServerTransport());
  // stdout is the MCP channel, so the startup diagnosis goes to stderr, where
  // clients surface it; the proxy stays up and reconnects when diffler starts
  void ensureUpstream().catch((err) => {
    process.stderr.write(`diffler-mcp: ${err.message ?? err}\n`);
  });
}

main().catch((err) => {
  process.stderr.write(`diffler-mcp: ${err.stack ?? err}\n`);
  process.exit(1);
});
