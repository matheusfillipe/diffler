#!/usr/bin/env bash
# Bump the version in every manifest, gate on `just ci`, then commit, tag and
# push. CI publishes from the committed manifest versions.
#
# Usage: scripts/release.sh <patch|minor|major>
set -euo pipefail

bump="${1:-}"
case "$bump" in
  patch | minor | major) ;;
  *)
    echo "usage: $0 <patch|minor|major>" >&2
    exit 2
    ;;
esac

if [ "$(git branch --show-current)" != "main" ]; then
  echo "release: must be on main" >&2
  exit 1
fi
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "release: working tree is dirty; commit or stash first" >&2
  exit 1
fi
git fetch -q origin main
if [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]; then
  echo "release: local main is not in sync with origin/main" >&2
  exit 1
fi

cur=$(grep -m1 -E '^version = "' Cargo.toml | sed -E 's/^version = "([^"]+)"/\1/')
IFS=. read -r major minor patch <<<"$cur"
case "$bump" in
  major) major=$((major + 1)); minor=0; patch=0 ;;
  minor) minor=$((minor + 1)); patch=0 ;;
  patch) patch=$((patch + 1)) ;;
esac
new="$major.$minor.$patch"
if git rev-parse "v$new" >/dev/null 2>&1; then
  echo "release: tag v$new already exists" >&2
  exit 1
fi
echo "release: $cur -> $new"

perl -0pi -e "s/^version = \"\Q$cur\E\"/version = \"$new\"/m" Cargo.toml
perl -0pi -e "s/(diffler-\w+ = \{ path = \"[^\"]+\", version = )\"\Q$cur\E\"/\${1}\"$new\"/g" Cargo.toml
perl -0pi -e "s/(\"version\": )\"[^\"]*\"/\${1}\"$new\"/" npm/diffler/package.json
perl -0pi -e "s/(\"version\": )\"[^\"]*\"/\${1}\"$new\"/" npm/diffler-mcp/package.json

# `just ci` also syncs Cargo.lock to the new version.
just ci
just package-check

git add Cargo.toml Cargo.lock npm/diffler/package.json npm/diffler-mcp/package.json
git commit -m "Release $new"
git tag "v$new"
git push origin main "v$new"
echo "release: pushed v$new; CI builds binaries and publishes crates.io + npm via OIDC"
