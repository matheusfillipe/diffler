#!/usr/bin/env node
"use strict";

// Launcher shim: the actual binary ships in per-platform packages installed
// through optionalDependencies (the esbuild distribution pattern). This file
// only resolves the right package and execs it.

const { spawnSync } = require("node:child_process");

const PACKAGES = {
  "linux-x64": "@matheusfillipe/diffler-linux-x64",
  "linux-arm64": "@matheusfillipe/diffler-linux-arm64",
  "darwin-x64": "@matheusfillipe/diffler-darwin-x64",
  "darwin-arm64": "@matheusfillipe/diffler-darwin-arm64",
  "win32-x64": "@matheusfillipe/diffler-win32-x64",
  "win32-arm64": "@matheusfillipe/diffler-win32-arm64",
};

function resolveBinary() {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PACKAGES[key];
  if (!pkg) {
    console.error(`diffler: unsupported platform ${key}`);
    console.error(`supported platforms: ${Object.keys(PACKAGES).join(", ")}`);
    process.exit(1);
  }
  const bin = process.platform === "win32" ? "bin/diffler.exe" : "bin/diffler";
  try {
    return require.resolve(`${pkg}/${bin}`);
  } catch {
    console.error(`diffler: platform package ${pkg} is not installed.`);
    console.error(
      "Reinstall with optional dependencies enabled: npm install -g @matheusfillipe/diffler"
    );
    process.exit(1);
  }
}

const result = spawnSync(resolveBinary(), process.argv.slice(2), {
  stdio: "inherit",
});
if (result.error) {
  console.error(`diffler: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
