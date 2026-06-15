"use strict";

// Resolve (and, if missing, fetch) the diffler binary for the current
// platform. The npm package ships only this JS; the actual binary is pulled
// from the matching GitHub release asset on install (and lazily on first run,
// so `--ignore-scripts` installs still work).

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const http = require("node:http");
const https = require("node:https");
const { execFileSync } = require("node:child_process");

const REPO = "matheusfillipe/diffler";
const BASE =
  process.env.DIFFLER_DOWNLOAD_BASE ||
  `https://github.com/${REPO}/releases/download`;

// node platform-arch -> { rust target triple, archive extension }
const TARGETS = {
  "darwin-arm64": { target: "aarch64-apple-darwin", ext: "tar.gz" },
  "darwin-x64": { target: "x86_64-apple-darwin", ext: "tar.gz" },
  "linux-x64": { target: "x86_64-unknown-linux-musl", ext: "tar.gz" },
  "linux-arm64": { target: "aarch64-unknown-linux-musl", ext: "tar.gz" },
  "win32-x64": { target: "x86_64-pc-windows-msvc", ext: "zip" },
  "win32-arm64": { target: "aarch64-pc-windows-msvc", ext: "zip" },
};

function binaryName() {
  return process.platform === "win32" ? "diffler.exe" : "diffler";
}

function binaryPath() {
  return path.join(__dirname, "..", "bin", binaryName());
}

function platformInfo() {
  const key = `${process.platform}-${process.arch}`;
  const info = TARGETS[key];
  if (!info) {
    throw new Error(
      `unsupported platform ${key} (supported: ${Object.keys(TARGETS).join(", ")})`,
    );
  }
  return info;
}

function assetUrl() {
  const { target, ext } = platformInfo();
  const version = require("../package.json").version;
  const asset = `diffler-v${version}-${target}.${ext}`;
  return { url: `${BASE}/v${version}/${asset}`, asset, ext };
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const get = (current, redirects) => {
      if (redirects > 5) {
        reject(new Error("too many redirects"));
        return;
      }
      const client = current.startsWith("http:") ? http : https;
      client
        .get(current, { headers: { "user-agent": "diffler-npm" } }, (res) => {
          const { statusCode, headers } = res;
          if (statusCode >= 300 && statusCode < 400 && headers.location) {
            res.resume();
            get(new URL(headers.location, current).toString(), redirects + 1);
            return;
          }
          if (statusCode !== 200) {
            res.resume();
            reject(new Error(`HTTP ${statusCode} for ${current}`));
            return;
          }
          const file = fs.createWriteStream(dest);
          res.pipe(file);
          file.on("finish", () => file.close(() => resolve()));
          file.on("error", reject);
        })
        .on("error", reject);
    };
    get(url, 0);
  });
}

function extract(archive, ext, into) {
  fs.mkdirSync(into, { recursive: true });
  if (ext === "zip") {
    // Expand-Archive is built into Windows PowerShell; the only platform that
    // ships a .zip asset is win32
    execFileSync("powershell", [
      "-NoProfile",
      "-Command",
      `Expand-Archive -LiteralPath '${archive}' -DestinationPath '${into}' -Force`,
    ]);
  } else {
    execFileSync("tar", ["-xzf", archive, "-C", into]);
  }
}

function findBinary(dir) {
  const want = binaryName();
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      const hit = findBinary(full);
      if (hit) return hit;
    } else if (entry.name === want) {
      return full;
    }
  }
  return null;
}

async function ensureBinary() {
  const dest = binaryPath();
  if (fs.existsSync(dest)) return dest;

  const { url, asset, ext } = assetUrl();
  const tmpArchive = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "diffler-")), asset);
  await download(url, tmpArchive);

  const extractDir = fs.mkdtempSync(path.join(os.tmpdir(), "diffler-"));
  extract(tmpArchive, ext, extractDir);

  const found = findBinary(extractDir);
  if (!found) {
    throw new Error(`binary ${binaryName()} not found inside ${asset}`);
  }
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.copyFileSync(found, dest);
  fs.chmodSync(dest, 0o755);
  return dest;
}

module.exports = { ensureBinary, binaryPath, assetUrl, platformInfo, findBinary, extract };
