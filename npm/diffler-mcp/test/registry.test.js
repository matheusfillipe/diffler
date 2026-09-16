// Cross-repo discovery: a proxy started from a directory that owns no
// .diffler/mcp.json of its own still finds a diffler running elsewhere,
// through the per-user instance registry the TUI publishes alongside it.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, utimesSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const PROXY = join(dirname(fileURLToPath(import.meta.url)), "..", "index.js");

function reply(repoLabel, method, params) {
  switch (method) {
    case "initialize":
      return {
        protocolVersion: params.protocolVersion,
        capabilities: { tools: {} },
        serverInfo: { name: "fake-diffler", version: "0" },
      };
    case "tools/list":
      return { tools: [{ name: "ping", description: "pong", inputSchema: { type: "object" } }] };
    case "tools/call":
      if (params.name === "review_status") {
        return { content: [{ type: "text", text: JSON.stringify({ repo: repoLabel }) }] };
      }
      return { content: [{ type: "text", text: "pong" }] };
    default:
      return {};
  }
}

// A fake diffler TUI: one HTTP backend plus its registry entry, standing in
// for the real process the Rust side would have started and registered.
async function startInstance(repo) {
  const http = createServer((req, res) => {
    if (req.method !== "POST") {
      res.writeHead(405).end();
      return;
    }
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const msg = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      if (msg.id === undefined) {
        res.writeHead(202).end();
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: reply(repo, msg.method, msg.params) }),
      );
    });
  });
  await new Promise((resolve) => http.listen(0, "127.0.0.1", resolve));
  return {
    repo,
    port: http.address().port,
    close: () => new Promise((resolve) => http.close(resolve)),
  };
}

// Like startInstance, but also answers the streamable-HTTP GET stream (the
// persistent one the client opens after the handshake, same as the real
// diffler MCP server) and holds it open. `streamSocket` names the exact
// connection it arrived on, so a test can prove the proxy closes that one.
async function startInstanceWithOpenStream(repo) {
  let streamSocket = null;
  const http = createServer((req, res) => {
    if (req.method === "GET") {
      streamSocket = req.socket;
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write(": open\n\n");
      return;
    }
    if (req.method !== "POST") {
      res.writeHead(405).end();
      return;
    }
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const msg = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      if (msg.id === undefined) {
        res.writeHead(202).end();
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: reply(repo, msg.method, msg.params) }),
      );
    });
  });
  await new Promise((resolve) => http.listen(0, "127.0.0.1", resolve));
  return {
    repo,
    port: http.address().port,
    streamSocket: () => streamSocket,
    close: () => {
      http.closeAllConnections();
      return new Promise((resolve) => http.close(resolve));
    },
  };
}

function driveProxy(cwd, args, env) {
  const child = spawn(process.execPath, [PROXY, ...args], {
    cwd,
    stdio: ["pipe", "pipe", "pipe"],
    env: { ...process.env, ...env },
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += chunk.toString("utf8");
  });
  const pending = new Map();
  let buffer = "";
  child.stdout.on("data", (chunk) => {
    buffer += chunk.toString("utf8");
    let nl;
    while ((nl = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, nl);
      buffer = buffer.slice(nl + 1);
      if (!line.trim()) continue;
      const msg = JSON.parse(line);
      if (msg.id !== undefined && pending.has(msg.id)) {
        pending.get(msg.id)(msg);
        pending.delete(msg.id);
      }
    }
  });
  let nextId = 0;
  const request = (method, params) => {
    const id = (nextId += 1);
    const result = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`timeout waiting for ${method}`));
      }, 5000);
      pending.set(id, (msg) => {
        clearTimeout(timer);
        resolve(msg);
      });
    });
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return result;
  };
  const notify = (method, params) =>
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  return { child, request, notify, stderr: () => stderr, kill: () => child.kill() };
}

async function handshake(proxy) {
  await proxy.request("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "test", version: "0" },
  });
  proxy.notify("notifications/initialized");
  return proxy;
}

const tmpDir = (prefix) => mkdtempSync(join(tmpdir(), prefix));

function registerInstance(stateDir, instance) {
  const dir = join(stateDir, "diffler", "instances");
  mkdirSync(dir, { recursive: true });
  writeFileSync(
    join(dir, `${instance.port}.json`),
    JSON.stringify({
      repo: instance.repo,
      port: instance.port,
      pid: instance.pid ?? process.pid,
      url: `http://127.0.0.1:${instance.port}/mcp`,
    }),
  );
}

// Registration order alone doesn't reliably set file mtime order on every
// filesystem, so tests that care which instance is "newest" set it explicitly.
function touchInstance(stateDir, instance, when) {
  const path = join(stateDir, "diffler", "instances", `${instance.port}.json`);
  utimesSync(path, when, when);
}

const callTool = (proxy, name, args = {}) => proxy.request("tools/call", { name, arguments: args });

async function waitUntil(predicate, message, timeoutMs = 2000) {
  const start = Date.now();
  while (!(await predicate())) {
    if (Date.now() - start > timeoutMs) {
      throw new Error(message);
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

test("a single live registry instance is used with no walk-up file", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const backend = await startInstance("/repos/widgets");
  registerInstance(stateDir, backend);

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const pong = await callTool(proxy, "ping");
    assert.equal(pong.result.content[0].text, "pong", "found the only registered instance");
  } finally {
    proxy.kill();
    await backend.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("a dead pid is pruned from the registry", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const backend = await startInstance("/repos/widgets");
  registerInstance(stateDir, { ...backend, pid: 999_999 });

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const call = await callTool(proxy, "ping");
    assert.equal(call.result.isError, true, "the dead registration doesn't count");
    assert.match(call.result.content[0].text, /no diffler is running/);
  } finally {
    proxy.kill();
    await backend.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("two live instances bind the newest, list_instances marks it current, tools/list carries its tools on the first call", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const widgets = await startInstance("/repos/widgets");
  const gadgets = await startInstance("/repos/gadgets");
  registerInstance(stateDir, widgets);
  registerInstance(stateDir, gadgets);
  // gadgets is the more recently started diffler
  touchInstance(stateDir, widgets, new Date(Date.now() - 60_000));
  touchInstance(stateDir, gadgets, new Date());

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const tools = await proxy.request("tools/list", {});
    assert.deepEqual(
      tools.result.tools.map((t) => t.name),
      ["list_instances", "use_instance", "ping"],
      "the newest instance's tools are there on the very first call",
    );

    const status = await callTool(proxy, "review_status");
    assert.deepEqual(JSON.parse(status.result.content[0].text), { repo: "/repos/gadgets" });

    const listed = await callTool(proxy, "list_instances");
    const instances = JSON.parse(listed.result.content[0].text).instances;
    const current = instances.filter((i) => i.current);
    assert.equal(current.length, 1, "exactly one instance is current");
    assert.equal(current[0].repo, "/repos/gadgets");

    assert.match(proxy.stderr(), /gadgets/);
    assert.match(proxy.stderr(), new RegExp(String(gadgets.port)));
    assert.match(proxy.stderr(), /use_instance/);
  } finally {
    proxy.kill();
    await widgets.close();
    await gadgets.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("the first forwarded result after an ambiguous auto-bind carries the bound-to notice, the second does not", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const widgets = await startInstance("/repos/widgets");
  const gadgets = await startInstance("/repos/gadgets");
  registerInstance(stateDir, widgets);
  registerInstance(stateDir, gadgets);
  touchInstance(stateDir, widgets, new Date(Date.now() - 60_000));
  touchInstance(stateDir, gadgets, new Date());

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const first = await callTool(proxy, "review_status");
    assert.deepEqual(
      first.result.content.map((c) => c.text),
      [
        JSON.stringify({ repo: "/repos/gadgets" }),
        `Bound to /repos/gadgets (port ${gadgets.port}) because several diffler instances are running; call use_instance to switch.`,
      ],
      "the ambiguous bind's first result carries one extra text block",
    );

    const second = await callTool(proxy, "review_status");
    assert.equal(second.result.content.length, 1, "the notice appears once only");
  } finally {
    proxy.kill();
    await widgets.close();
    await gadgets.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("use_instance resets a pending ambiguous-bind notice, so the fresh binding's first call carries none", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const widgets = await startInstance("/repos/widgets");
  const gadgets = await startInstance("/repos/gadgets");
  registerInstance(stateDir, widgets);
  registerInstance(stateDir, gadgets);
  touchInstance(stateDir, widgets, new Date(Date.now() - 60_000));
  touchInstance(stateDir, gadgets, new Date());

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    // tools/list connects ambiguously (binding gadgets) but never consumes
    // the notice: only a forwarded tool result does that
    await proxy.request("tools/list", {});

    const picked = await callTool(proxy, "use_instance", { repo: "widgets" });
    assert.equal(picked.result.isError, undefined, picked.result.content?.[0]?.text);

    const status = await callTool(proxy, "review_status");
    assert.equal(
      status.result.content.length,
      1,
      "the explicit use_instance is not ambiguous, so it clears the notice the earlier auto-bind left pending",
    );
    assert.deepEqual(JSON.parse(status.result.content[0].text), { repo: "/repos/widgets" });
  } finally {
    proxy.kill();
    await widgets.close();
    await gadgets.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("an older walk-up match outranks a newer registry instance", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const widgets = await startInstance("/repos/widgets");
  const gadgets = await startInstance("/repos/gadgets");
  registerInstance(stateDir, widgets);
  registerInstance(stateDir, gadgets);
  // gadgets is the more recently started diffler, but cwd's own endpoint
  // file names widgets, the older one
  touchInstance(stateDir, widgets, new Date(Date.now() - 60_000));
  touchInstance(stateDir, gadgets, new Date());
  mkdirSync(join(cwd, ".diffler"), { recursive: true });
  writeFileSync(join(cwd, ".diffler", "mcp.json"), JSON.stringify({ port: widgets.port }));

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const status = await callTool(proxy, "review_status");
    assert.deepEqual(
      JSON.parse(status.result.content[0].text),
      { repo: "/repos/widgets" },
      "the walk-up match wins even though the registry's newest is a different instance",
    );
  } finally {
    proxy.kill();
    await widgets.close();
    await gadgets.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("use_instance by directory name retargets the proxy", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const widgets = await startInstance("/repos/widgets");
  const gadgets = await startInstance("/repos/gadgets");
  registerInstance(stateDir, widgets);
  registerInstance(stateDir, gadgets);

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const picked = await callTool(proxy, "use_instance", { repo: "gadgets" });
    assert.equal(picked.result.isError, undefined, picked.result.content?.[0]?.text);
    const body = JSON.parse(picked.result.content[0].text);
    assert.deepEqual(body, { ok: true, repo: "/repos/gadgets", port: gadgets.port });

    const status = await callTool(proxy, "review_status");
    assert.deepEqual(JSON.parse(status.result.content[0].text), { repo: "/repos/gadgets" });
  } finally {
    proxy.kill();
    await widgets.close();
    await gadgets.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("use_instance closes the connection to the instance it switches away from", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const widgets = await startInstanceWithOpenStream("/repos/widgets");
  const gadgets = await startInstanceWithOpenStream("/repos/gadgets");
  registerInstance(stateDir, widgets);
  registerInstance(stateDir, gadgets);
  // widgets is the more recently started diffler, so auto-selection binds
  // to it first, leaving gadgets as the instance to switch to below
  touchInstance(stateDir, gadgets, new Date(Date.now() - 60_000));
  touchInstance(stateDir, widgets, new Date());

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    await callTool(proxy, "ping"); // binds to widgets, the newest instance
    // the client opens its persistent stream in the background, after the
    // handshake, so give it a moment to actually land on the server
    await waitUntil(() => widgets.streamSocket() !== null, "widgets never saw the open stream");
    const widgetsSocket = widgets.streamSocket();
    assert.equal(widgetsSocket.destroyed, false, "widgets' stream is open");

    const picked = await callTool(proxy, "use_instance", { repo: "gadgets" });
    assert.equal(picked.result.isError, undefined, picked.result.content?.[0]?.text);

    await waitUntil(
      () => widgetsSocket.destroyed,
      "switching instance closes the connection it leaves behind, not just the reference",
    );
    await waitUntil(() => gadgets.streamSocket() !== null, "gadgets never got its own open stream");
    assert.equal(gadgets.streamSocket().destroyed, false, "the new instance's own connection stays open");
  } finally {
    proxy.kill();
    await widgets.close();
    await gadgets.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("list_instances then use_instance from a third, unrelated directory", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const repoA = tmpDir("diffler-mcp-repoA-");
  const repoB = tmpDir("diffler-mcp-repoB-");
  const outsider = tmpDir("diffler-mcp-outsider-");
  const tuiA = await startInstance(repoA);
  const tuiB = await startInstance(repoB);
  registerInstance(stateDir, tuiA);
  registerInstance(stateDir, tuiB);

  // the proxy runs from a directory in neither repo, with no local
  // .diffler/mcp.json anywhere up its own tree
  const proxy = await handshake(driveProxy(outsider, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const listed = await callTool(proxy, "list_instances");
    const instances = JSON.parse(listed.result.content[0].text).instances;
    assert.deepEqual(
      instances.map((i) => i.repo).sort(),
      [repoA, repoB].sort(),
      "both TUIs are visible from the third directory",
    );

    const targetName = repoB.split("/").pop();
    const picked = await callTool(proxy, "use_instance", { repo: targetName });
    assert.equal(picked.result.isError, undefined, picked.result.content?.[0]?.text);

    const status = await callTool(proxy, "review_status");
    assert.deepEqual(JSON.parse(status.result.content[0].text), { repo: repoB });
  } finally {
    proxy.kill();
    await tuiA.close();
    await tuiB.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(repoA, { recursive: true, force: true });
    rmSync(repoB, { recursive: true, force: true });
    rmSync(outsider, { recursive: true, force: true });
  }
});

test("use_instance to a dead port errors and keeps the previous binding", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const widgets = await startInstance("/repos/widgets");
  const dead = await startInstance("/repos/dead");
  registerInstance(stateDir, widgets);
  // the registered pid is this test process, which always looks alive, so
  // the registry alone can't tell this instance died; only a real connect can
  registerInstance(stateDir, { ...dead, pid: process.pid });
  await dead.close();

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const picked = await callTool(proxy, "use_instance", { repo: "widgets" });
    assert.equal(picked.result.isError, undefined, picked.result.content?.[0]?.text);

    const failed = await callTool(proxy, "use_instance", { repo: "dead" });
    assert.equal(failed.result.isError, true, "a dead instance fails to connect");
    assert.match(failed.result.content[0].text, /dead/);
    assert.match(failed.result.content[0].text, new RegExp(String(dead.port)));

    const status = await callTool(proxy, "review_status");
    assert.deepEqual(
      JSON.parse(status.result.content[0].text),
      { repo: "/repos/widgets" },
      "the previous binding still answers",
    );
  } finally {
    proxy.kill();
    await widgets.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("current in list_instances follows use_instance", async () => {
  const stateDir = tmpDir("diffler-mcp-state-");
  const cwd = tmpDir("diffler-mcp-cwd-");
  const widgets = await startInstance("/repos/widgets");
  const gadgets = await startInstance("/repos/gadgets");
  registerInstance(stateDir, widgets);
  registerInstance(stateDir, gadgets);

  const proxy = await handshake(driveProxy(cwd, [], { DIFFLER_STATE_DIR: stateDir }));
  try {
    const picked = await callTool(proxy, "use_instance", { repo: "gadgets" });
    assert.equal(picked.result.isError, undefined, picked.result.content?.[0]?.text);

    const listed = await callTool(proxy, "list_instances");
    const instances = JSON.parse(listed.result.content[0].text).instances;
    const current = instances.filter((i) => i.current);
    assert.equal(current.length, 1, "exactly one instance is current");
    assert.equal(current[0].repo, "/repos/gadgets");
  } finally {
    proxy.kill();
    await widgets.close();
    await gadgets.close();
    rmSync(stateDir, { recursive: true, force: true });
    rmSync(cwd, { recursive: true, force: true });
  }
});
