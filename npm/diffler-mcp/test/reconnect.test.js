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

function driveProxy(repo) {
  const child = spawn(process.execPath, [PROXY, "--repo", repo], {
    cwd: repo,
    stdio: ["pipe", "pipe", "inherit"],
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
  return { child, request, notify, kill: () => child.kill() };
}

const writeEndpoint = (repo, port) =>
  writeFileSync(join(repo, ".diffler", "mcp.json"), JSON.stringify({ port }));

const callPing = (proxy) => proxy.request("tools/call", { name: "ping", arguments: {} });

test("proxy bridges, survives diffler restart on a new port", async () => {
  const repo = mkdtempSync(join(tmpdir(), "diffler-mcp-"));
  mkdirSync(join(repo, ".diffler"));
  let backend = await startBackend();
  writeEndpoint(repo, backend.port);

  const proxy = driveProxy(repo);
  try {
    await proxy.request("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "test", version: "0" },
    });
    proxy.notify("notifications/initialized");

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
