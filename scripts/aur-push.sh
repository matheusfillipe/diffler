#!/usr/bin/env bash
# Push the repo's packaging/aur/{PKGBUILD,.SRCINFO} to the AUR. Run locally — it
# uses your aur@aur.archlinux.org SSH key. CI keeps those files version-synced on
# every release; this publishes them whenever you choose.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
ver=$(grep -m1 '^pkgver=' "$root/packaging/aur/PKGBUILD" | cut -d= -f2)
# CI commits the rendered PKGBUILD to main after the tag push, so a checkout
# that predates it would silently publish the previous version
latest=$(git -C "$root" tag --list 'v*' --sort=-v:refname | head -1)
if [ "v$ver" != "$latest" ]; then
  echo "aur: PKGBUILD is $ver but the latest tag is $latest — git pull first" >&2
  exit 1
fi
remote="ssh://aur@aur.archlinux.org/diffler-bin.git"
work=$(mktemp -d)

if ! git clone -q "$remote" "$work" 2>/dev/null; then
  git -C "$work" init -q -b master
  git -C "$work" remote add origin "$remote"
fi
cp "$root/packaging/aur/PKGBUILD" "$root/packaging/aur/.SRCINFO" "$work/"
git -C "$work" add PKGBUILD .SRCINFO
if git -C "$work" diff --cached --quiet; then
  echo "aur: already up to date at $ver"
  exit 0
fi
git -C "$work" commit -q -m "diffler-bin $ver"
git -C "$work" push -q origin HEAD:master
echo "aur: pushed diffler-bin $ver"
