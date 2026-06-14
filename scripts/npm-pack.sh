#!/usr/bin/env bash
# Repack release archives into npm packages under npm/dist/:
# one @mattfillipe/diffler-<platform> package per archive found, plus the
# @mattfillipe/diffler entry package (launcher shim + optionalDependencies).
#
# Usage: scripts/npm-pack.sh <version> <archives-dir>
#   <archives-dir> holds diffler-<tag>-<target>.{tar.gz,zip} release assets.
#   Missing targets are skipped, so a single-platform dry run works too.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <version> <archives-dir>" >&2
  exit 2
fi

version=$1
archives=$2
root=$(cd "$(dirname "$0")/.." && pwd)
dist="$root/npm/dist"

targets="
x86_64-unknown-linux-musl:linux-x64
aarch64-unknown-linux-musl:linux-arm64
x86_64-apple-darwin:darwin-x64
aarch64-apple-darwin:darwin-arm64
x86_64-pc-windows-msvc:win32-x64
aarch64-pc-windows-msvc:win32-arm64
"

rm -rf "$dist"
mkdir -p "$dist"
packed=0

for pair in $targets; do
  target=${pair%%:*}
  platform=${pair##*:}
  archive=$(find "$archives" -maxdepth 1 \( -name "diffler-*-$target.tar.gz" -o -name "diffler-*-$target.zip" \) | head -n 1)
  [[ -n "$archive" ]] || continue

  tmp=$(mktemp -d)
  case "$archive" in
    *.tar.gz) tar xzf "$archive" -C "$tmp" ;;
    *.zip) unzip -q "$archive" -d "$tmp" ;;
  esac

  if [[ "$target" == *windows* ]]; then bin_name=diffler.exe; else bin_name=diffler; fi
  binary=$(find "$tmp" -type f -name "$bin_name" | head -n 1)
  if [[ -z "$binary" ]]; then
    echo "error: no $bin_name inside $archive" >&2
    exit 1
  fi

  pkg_dir="$dist/@mattfillipe/diffler-$platform"
  mkdir -p "$pkg_dir/bin"
  cp "$binary" "$pkg_dir/bin/$bin_name"
  chmod +x "$pkg_dir/bin/$bin_name"
  rm -rf "$tmp"

  node -e '
    const fs = require("node:fs");
    const [tmpl, name, version, platform, out] = process.argv.slice(1);
    const pkg = JSON.parse(fs.readFileSync(tmpl, "utf8"));
    const [os, cpu] = platform.split("-");
    pkg.name = name;
    pkg.version = version;
    pkg.description = `diffler binary for ${platform}`;
    pkg.os = [os];
    pkg.cpu = [cpu];
    fs.writeFileSync(out, JSON.stringify(pkg, null, 2) + "\n");
  ' "$root/npm/platform/package.json" "@mattfillipe/diffler-$platform" "$version" "$platform" "$pkg_dir/package.json"

  echo "packed @mattfillipe/diffler-$platform ($archive)"
  packed=$((packed + 1))
done

if [[ $packed -eq 0 ]]; then
  echo "error: no release archives found in $archives" >&2
  exit 1
fi

mkdir -p "$dist/diffler/bin"
cp "$root/npm/diffler/bin/diffler.js" "$dist/diffler/bin/"
cp "$root/npm/diffler/README.md" "$dist/diffler/"
node -e '
  const fs = require("node:fs");
  const [tmpl, version, out] = process.argv.slice(1);
  const pkg = JSON.parse(fs.readFileSync(tmpl, "utf8"));
  pkg.version = version;
  for (const dep of Object.keys(pkg.optionalDependencies)) {
    pkg.optionalDependencies[dep] = version;
  }
  fs.writeFileSync(out, JSON.stringify(pkg, null, 2) + "\n");
' "$root/npm/diffler/package.json" "$version" "$dist/diffler/package.json"
echo "packed diffler entry package ($packed platform packages, version $version)"
