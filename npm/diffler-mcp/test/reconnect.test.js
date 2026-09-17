import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const PROXY = join(dirname(fileURLToPath(import.meta.url)), "..", "index.js");

function reply(method, params) {
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
      return { content: [{ type: "text", text: "pong" }] };
    default:
      return {};
  }
}

async function startBackend() {
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
      res.end(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: reply(msg.method, msg.params) }));
    });
  });
  await new Promise((resolve) => http.listen(0, "127.0.0.1", resolve));
  return {
    port: http.address().port,
    close: () => new Promise((resolve) => http.close(resolve)),
  };
}

// Answers initialize and the first two tools/list calls (the connect
// handshake's own liveness check, then the first tools/list this proxy
// forwards) normally, then hangs on every tools/list after that: standing
// in for a diffler that accepted the connection and then hung.
async function startBackendThatHangsOnASecondToolsList() {
  let toolsListCalls = 0;
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
      if (msg.method === "tools/list") {
        toolsListCalls += 1;
        if (toolsListCalls > 2) {
          return;
        }
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: reply(msg.method, msg.params) }));
    });
  });
  await new Promise((resolve) => http.listen(0, "127.0.0.1", resolve));
  return {
    port: http.address().port,
    close: () => {
      http.closeAllConnections();
      return new Promise((resolve) => http.close(resolve));
    },
  };
}

// Answers initialize and tools/list normally (so the proxy connects and caches
// it) but never responds to a tools/call, standing in for a diffler that hung.
// `hungSockets` names the exact connection each hung request arrived on, so a
// test can prove the proxy closes that one rather than counting connections
// in a pool the initialize handshake also uses.
async function startHangingBackend() {
  const hungSockets = [];
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
      if (msg.method === "tools/call") {
        hungSockets.push(req.socket);
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: reply(msg.method, msg.params) }));
    });
  });
  await new Promise((resolve) => http.listen(0, "127.0.0.1", resolve));
  return {
    port: http.address().port,
    hungSockets,
    // The hung request's connection never ends on its own, so a graceful
    // close() would wait on it forever; force it closed instead.
    close: () => {
      http.closeAllConnections();
      return new Promise((resolve) => http.close(resolve));
    },
  };
}

// Every proxy gets its own empty registry dir unless the caller passes one,
// so a test never sees (or pollutes) whatever real diffler happens to be
// running on the machine this suite executes on.
function driveProxy(cwd, args = ["--repo", cwd], env = {}) {
  const ownedStateDir = env.DIFFLER_STATE_DIR ? null : mkdtempSync(join(tmpdir(), "diffler-mcp-state-"));
  const child = spawn(process.execPath, [PROXY, ...args], {
    cwd,
    stdio: ["pipe", "pipe", "pipe"],
    env: { ...process.env, ...env, DIFFLER_STATE_DIR: env.DIFFLER_STATE_DIR ?? ownedStateDir },
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += chunk.toString("utf8");
  });
  const pending = new Map();
  const notificationWaiters = [];
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
      } else if (msg.id === undefined && msg.method) {
        for (const waiter of notificationWaiters.splice(0)) {
          waiter(msg);
        }
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
  const waitForNotification = (timeoutMs = 5000) =>
    new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("timeout waiting for a notification")), timeoutMs);
      notificationWaiters.push((msg) => {
        clearTimeout(timer);
        resolve(msg);
      });
    });
  const kill = () => {
    child.kill();
    if (ownedStateDir) {
      rmSync(ownedStateDir, { recursive: true, force: true });
    }
  };
  return { child, request, notify, waitForNotification, stderr: () => stderr, kill };
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

const tmpRepo = () => mkdtempSync(join(tmpdir(), "diffler-mcp-"));

const writeEndpoint = (repo, port) => {
  mkdirSync(join(repo, ".diffler"), { recursive: true });
  writeFileSync(join(repo, ".diffler", "mcp.json"), JSON.stringify({ port }));
};

const callPing = (proxy) => proxy.request("tools/call", { name: "ping", arguments: {} });

async function waitUntil(predicate, message, timeoutMs = 2000) {
  const start = Date.now();
  while (!(await predicate())) {
    if (Date.now() - start > timeoutMs) {
      throw new Error(message);
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

test("proxy bridges, survives diffler restart on a new port", async () => {
  const repo = tmpRepo();
  let backend = await startBackend();
  writeEndpoint(repo, backend.port);

  const proxy = await handshake(driveProxy(repo));
  try {
    const tools = await proxy.request("tools/list", {});
    assert.deepEqual(
      tools.result.tools.map((t) => t.name),
      ["list_instances", "use_instance", "ping"],
      "the proxy's own tools plus whatever diffler forwards",
    );

    const up = await callPing(proxy);
    assert.equal(up.result.content[0].text, "pong");

    await backend.close();
    const down = await callPing(proxy);
    assert.equal(down.result.isError, true, "tool call reports diffler is down");
    assert.match(down.result.content[0].text, /isn't reachable/);

    backend = await startBackend();
    writeEndpoint(repo, backend.port);
    const again = await callPing(proxy);
    assert.equal(again.result.content[0].text, "pong", "reconnected after restart");
  } finally {
    proxy.kill();
    await backend.close().catch(() => {});
    rmSync(repo, { recursive: true, force: true });
  }
});

test("discovers the endpoint from a nested subdirectory", async () => {
  const repo = tmpRepo();
  const nested = join(repo, "crates", "diffler", "src");
  mkdirSync(nested, { recursive: true });
  const backend = await startBackend();
  writeEndpoint(repo, backend.port);

  // no --repo: the editor's cwd is all the proxy gets
  const proxy = await handshake(driveProxy(nested, []));
  try {
    const pong = await callPing(proxy);
    assert.equal(pong.result.content[0].text, "pong", "walked up to the repo root");
  } finally {
    proxy.kill();
    await backend.close();
    rmSync(repo, { recursive: true, force: true });
  }
});

test("an explicit port wins over discovery", async () => {
  const repo = tmpRepo();
  const backend = await startBackend();
  const stale = await startBackend();
  await stale.close();
  writeEndpoint(repo, stale.port);

  const proxy = await handshake(driveProxy(repo, ["--port", String(backend.port)]));
  try {
    const pong = await callPing(proxy);
    assert.equal(pong.result.content[0].text, "pong", "used the flag, not the endpoint file");
  } finally {
    proxy.kill();
    await backend.close();
    rmSync(repo, { recursive: true, force: true });
  }
});

test("no endpoint file anywhere up the tree names the directory", async () => {
  const dir = tmpRepo();
  const proxy = await handshake(driveProxy(dir, []));
  try {
    const tools = await proxy.request("tools/list", {});
    assert.deepEqual(
      tools.result.tools.map((t) => t.name),
      ["list_instances", "use_instance"],
      "only the proxy's own tools when diffler is unreachable",
    );

    const call = await callPing(proxy);
    assert.equal(call.result.isError, true);
    assert.match(call.result.content[0].text, /no diffler is running in .* or any parent/);
    assert.match(proxy.stderr(), /no diffler is running in .* or any parent/);
  } finally {
    proxy.kill();
    rmSync(dir, { recursive: true, force: true });
  }
});

test("retries a missing diffler in the background and announces its tools once it appears", async () => {
  const repo = tmpRepo();
  const proxy = await handshake(driveProxy(repo, ["--repo", repo], { DIFFLER_MCP_RETRY_MS: "50" }));
  let backend;
  try {
    const before = await proxy.request("tools/list", {});
    assert.deepEqual(
      before.result.tools.map((t) => t.name),
      ["list_instances", "use_instance"],
      "only the proxy's own tools while diffler is not running yet",
    );

    const changed = proxy.waitForNotification();
    backend = await startBackend();
    writeEndpoint(repo, backend.port);
    const notification = await changed;
    assert.equal(notification.method, "notifications/tools/list_changed");

    const after = await proxy.request("tools/list", {});
    assert.deepEqual(
      after.result.tools.map((t) => t.name),
      ["list_instances", "use_instance", "ping"],
      "diffler's tools appear once it starts",
    );
  } finally {
    proxy.kill();
    await backend?.close().catch(() => {});
    rmSync(repo, { recursive: true, force: true });
  }
});

test("a call that never answers fails at the deadline, then reconnects on the next call", async () => {
  const repo = tmpRepo();
  let backend = await startHangingBackend();
  writeEndpoint(repo, backend.port);

  const proxy = await handshake(driveProxy(repo, ["--repo", repo], { DIFFLER_MCP_CALL_DEADLINE_MS: "50" }));
  try {
    const tools = await proxy.request("tools/list", {});
    assert.deepEqual(tools.result.tools.map((t) => t.name), ["list_instances", "use_instance", "ping"]);

    const started = Date.now();
    const hung = await callPing(proxy);
    assert.equal(hung.result.isError, true, "a hung call reports an error at the deadline");
    assert.match(hung.result.content[0].text, /did not answer/);
    assert.match(hung.result.content[0].text, new RegExp(String(backend.port)), "names the instance's port");
    assert.ok(Date.now() - started < 3000, "fails at the deadline, not the test's own request timeout");

    await backend.close();
    backend = await startBackend();
    writeEndpoint(repo, backend.port);
    const again = await callPing(proxy);
    assert.equal(again.result.content[0].text, "pong", "reconnected on the next call");
  } finally {
    proxy.kill();
    await backend.close().catch(() => {});
    rmSync(repo, { recursive: true, force: true });
  }
});

test("tools/list against a hung backend fails at the deadline too", async () => {
  const repo = tmpRepo();
  const backend = await startBackendThatHangsOnASecondToolsList();
  writeEndpoint(repo, backend.port);

  const proxy = await handshake(driveProxy(repo, ["--repo", repo], { DIFFLER_MCP_CALL_DEADLINE_MS: "50" }));
  try {
    // connects: the handshake's own liveness check is the backend's one live answer
    const first = await proxy.request("tools/list", {});
    assert.deepEqual(first.result.tools.map((t) => t.name), ["list_instances", "use_instance", "ping"]);

    const started = Date.now();
    const second = await proxy.request("tools/list", {});
    assert.deepEqual(
      second.result.tools.map((t) => t.name),
      ["list_instances", "use_instance"],
      "the hung backend's own tools drop out once its tools/list misses the deadline",
    );
    assert.ok(Date.now() - started < 3000, "fails at the deadline, not the test's own request timeout");
  } finally {
    proxy.kill();
    await backend.close();
    rmSync(repo, { recursive: true, force: true });
  }
});

test("a call that never answers closes the abandoned connection at the deadline", async () => {
  const repo = tmpRepo();
  const backend = await startHangingBackend();
  writeEndpoint(repo, backend.port);

  const proxy = await handshake(driveProxy(repo, ["--repo", repo], { DIFFLER_MCP_CALL_DEADLINE_MS: "50" }));
  try {
    await proxy.request("tools/list", {});

    const hung = await callPing(proxy);
    assert.equal(hung.result.isError, true);
    assert.equal(backend.hungSockets.length, 1, "the hung call reached the backend");

    await waitUntil(
      () => backend.hungSockets[0].destroyed,
      "the proxy closes the connection it abandoned at the deadline, not just the reference",
    );
  } finally {
    proxy.kill();
    await backend.close().catch(() => {});
    rmSync(repo, { recursive: true, force: true });
  }
});

test("the background retry stops at its ceiling", async () => {
  const repo = tmpRepo();
  const proxy = await handshake(
    driveProxy(repo, ["--repo", repo], { DIFFLER_MCP_RETRY_MS: "20", DIFFLER_MCP_RETRY_MAX_MS: "150" }),
  );
  let backend;
  try {
    const before = await proxy.request("tools/list", {});
    assert.deepEqual(
      before.result.tools.map((t) => t.name),
      ["list_instances", "use_instance"],
      "diffler isn't running yet; this call also arms the background retry",
    );

    // outlive the ceiling with no further request, so only the background
    // loop could still be trying
    await new Promise((resolve) => setTimeout(resolve, 300));

    backend = await startBackend();
    writeEndpoint(repo, backend.port);

    await assert.rejects(
      proxy.waitForNotification(200),
      /timeout waiting for a notification/,
      "the retry loop already gave up at its ceiling, so diffler appearing now goes unnoticed",
    );
  } finally {
    proxy.kill();
    await backend?.close().catch(() => {});
    rmSync(repo, { recursive: true, force: true });
  }
});
