#!/usr/bin/env bash
# Render the Homebrew formula, Scoop manifest, AUR PKGBUILD/.SRCINFO, and Nix
# flake for a release from its uploaded GitHub assets. CI commits all of them;
# the AUR push is done manually (just aur-publish). The binary archives carry a
# top-level diffler-v<ver>-<target>/ directory holding the `diffler` binary.
#
# Usage: scripts/packaging.sh <version> <assets-dir>
set -euo pipefail

ver="${1:?usage: $0 <version> <assets-dir>}"
assets="${2:?usage: $0 <version> <assets-dir>}"
repo="https://github.com/matheusfillipe/diffler"
base="$repo/releases/download/v$ver"
desc="Terminal code review for AI coding agents"
root=$(cd "$(dirname "$0")/.." && pwd)

sha() {
  local f="$assets/diffler-v$ver-$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$f" | cut -d' ' -f1
  else
    shasum -a 256 "$f" | cut -d' ' -f1
  fi
}

mac_arm=$(sha "aarch64-apple-darwin.tar.gz")
mac_x64=$(sha "x86_64-apple-darwin.tar.gz")
lin_arm=$(sha "aarch64-unknown-linux-musl.tar.gz")
lin_x64=$(sha "x86_64-unknown-linux-musl.tar.gz")
win_x64=$(sha "x86_64-pc-windows-msvc.zip")
win_arm=$(sha "aarch64-pc-windows-msvc.zip")

mkdir -p "$root/Formula" "$root/bucket" "$root/packaging/aur"

cat >"$root/Formula/diffler.rb" <<RB
class Diffler < Formula
  desc "$desc"
  homepage "$repo"
  version "$ver"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "$base/diffler-v$ver-aarch64-apple-darwin.tar.gz"
      sha256 "$mac_arm"
    end
    on_intel do
      url "$base/diffler-v$ver-x86_64-apple-darwin.tar.gz"
      sha256 "$mac_x64"
    end
  end

  on_linux do
    on_arm do
      url "$base/diffler-v$ver-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$lin_arm"
    end
    on_intel do
      url "$base/diffler-v$ver-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$lin_x64"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
RB

cat >"$root/bucket/diffler.json" <<JSON
{
  "version": "$ver",
  "description": "$desc",
  "homepage": "$repo",
  "license": "MIT OR Apache-2.0",
  "architecture": {
    "64bit": {
      "url": "$base/diffler-v$ver-x86_64-pc-windows-msvc.zip",
      "hash": "$win_x64",
      "extract_dir": "diffler-v$ver-x86_64-pc-windows-msvc"
    },
    "arm64": {
      "url": "$base/diffler-v$ver-aarch64-pc-windows-msvc.zip",
      "hash": "$win_arm",
      "extract_dir": "diffler-v$ver-aarch64-pc-windows-msvc"
    }
  },
  "bin": "diffler.exe"
}
JSON

cat >"$root/packaging/aur/PKGBUILD" <<PKG
# Maintainer: Matheus Fillipe <matheus.fillipe@syte.ms>
pkgname=diffler-bin
pkgver=$ver
pkgrel=1
pkgdesc="$desc"
arch=('x86_64' 'aarch64')
url="$repo"
license=('MIT' 'Apache-2.0')
provides=('diffler')
conflicts=('diffler')
source_x86_64=("diffler-\$pkgver-x86_64.tar.gz::$base/diffler-v$ver-x86_64-unknown-linux-musl.tar.gz")
source_aarch64=("diffler-\$pkgver-aarch64.tar.gz::$base/diffler-v$ver-aarch64-unknown-linux-musl.tar.gz")
sha256sums_x86_64=('$lin_x64')
sha256sums_aarch64=('$lin_arm')

package() {
  local triple
  case "\$CARCH" in
    x86_64) triple="x86_64-unknown-linux-musl" ;;
    aarch64) triple="aarch64-unknown-linux-musl" ;;
  esac
  install -Dm755 "diffler-v$ver-\$triple/diffler" "\$pkgdir/usr/bin/diffler"
  install -Dm644 "diffler-v$ver-\$triple/LICENSE-MIT" "\$pkgdir/usr/share/licenses/\$pkgname/LICENSE-MIT"
  install -Dm644 "diffler-v$ver-\$triple/LICENSE-APACHE" "\$pkgdir/usr/share/licenses/\$pkgname/LICENSE-APACHE"
}
PKG

# .SRCINFO is normally produced by `makepkg --printsrcinfo` (Arch-only); since
# every field is known here, render it directly so the AUR push needs no makepkg
{
  printf 'pkgbase = diffler-bin\n'
  printf '\tpkgdesc = %s\n' "$desc"
  printf '\tpkgver = %s\n' "$ver"
  printf '\tpkgrel = 1\n'
  printf '\turl = %s\n' "$repo"
  printf '\tarch = x86_64\n\tarch = aarch64\n'
  printf '\tlicense = MIT\n\tlicense = Apache-2.0\n'
  printf '\tprovides = diffler\n\tconflicts = diffler\n'
  printf '\tsource_x86_64 = diffler-%s-x86_64.tar.gz::%s/diffler-v%s-x86_64-unknown-linux-musl.tar.gz\n' "$ver" "$base" "$ver"
  printf '\tsha256sums_x86_64 = %s\n' "$lin_x64"
  printf '\tsource_aarch64 = diffler-%s-aarch64.tar.gz::%s/diffler-v%s-aarch64-unknown-linux-musl.tar.gz\n' "$ver" "$base" "$ver"
  printf '\tsha256sums_aarch64 = %s\n' "$lin_arm"
  printf '\npkgname = diffler-bin\n'
} >"$root/packaging/aur/.SRCINFO"

# Flake fetching the prebuilt binary (musl-static on Linux runs on NixOS as-is,
# so no patchelf). nix's ${..} and $out are escaped to survive this heredoc.
cat >"$root/flake.nix" <<NIX
{
  description = "$desc";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "$ver";
      base = "$base";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "$lin_x64"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "$lin_arm"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "$mac_x64"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "$mac_arm"; };
      };
      forAllSystems = nixpkgs.lib.genAttrs (builtins.attrNames targets);
    in {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.\${system};
          t = targets.\${system};
        in {
          default = pkgs.stdenvNoCC.mkDerivation {
            pname = "diffler";
            inherit version;
            src = pkgs.fetchurl {
              url = "\${base}/diffler-v\${version}-\${t.triple}.tar.gz";
              sha256 = t.sha256;
            };
            sourceRoot = ".";
            dontStrip = true;
            installPhase = ''
              install -Dm755 diffler-v\${version}-\${t.triple}/diffler \$out/bin/diffler
            '';
          };
        });
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "\${self.packages.\${system}.default}/bin/diffler";
        };
      });
    };
}
NIX

echo "rendered Formula/diffler.rb, bucket/diffler.json, packaging/aur/{PKGBUILD,.SRCINFO}, flake.nix for $ver"
