#!/usr/bin/env node
"use strict";

// Launcher shim: resolve the platform binary (fetching it on first run if the
// postinstall step was skipped), then exec it with the caller's args/stdio.

const { spawnSync } = require("node:child_process");
const { ensureBinary } = require("../lib/resolve.js");

async function main() {
  let binary;
  try {
    binary = await ensureBinary();
  } catch (err) {
    process.stderr.write(`diffler: ${err.message}\n`);
    process.exit(1);
  }
  const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
  if (result.error) {
    process.stderr.write(`diffler: ${result.error.message}\n`);
    process.exit(1);
  }
  process.exit(result.status === null ? 1 : result.status);
}

main();
