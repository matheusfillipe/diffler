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

function driveProxy(cwd, args = ["--repo", cwd]) {
  const child = spawn(process.execPath, [PROXY, ...args], {
    cwd,
    stdio: ["pipe", "pipe", "pipe"],
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

const tmpRepo = () => mkdtempSync(join(tmpdir(), "diffler-mcp-"));

const writeEndpoint = (repo, port) => {
  mkdirSync(join(repo, ".diffler"), { recursive: true });
  writeFileSync(join(repo, ".diffler", "mcp.json"), JSON.stringify({ port }));
};

const callPing = (proxy) => proxy.request("tools/call", { name: "ping", arguments: {} });

test("proxy bridges, survives diffler restart on a new port", async () => {
  const repo = tmpRepo();
  let backend = await startBackend();
  writeEndpoint(repo, backend.port);

  const proxy = await handshake(driveProxy(repo));
  try {
    const tools = await proxy.request("tools/list", {});
    assert.deepEqual(
      tools.result.tools.map((t) => t.name),
      ["ping"],
      "tools forwarded while diffler is up",
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
    assert.deepEqual(tools.result.tools, [], "nothing to forward");

    const call = await callPing(proxy);
    assert.equal(call.result.isError, true);
    assert.match(call.result.content[0].text, /no diffler is running in .* or any parent/);
    assert.match(proxy.stderr(), /no diffler is running in .* or any parent/);
  } finally {
    proxy.kill();
    rmSync(dir, { recursive: true, force: true });
  }
});
