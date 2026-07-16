{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "883b1b3f58346cd1a1bfe7aaf5113bf079c35e5351429b19797c204e48967a4f"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "0a0b405213f1d4270fb630b3a648acd6deb15c636c1281438cfd13524a1c2362"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "e9b185489527a56b3190117f5cf915cb3f290511772432049fa6f1268273f1e5"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "4da1aff264ebf04b014cb5168f7355b3f07c785e845e7fde73600fc1e2323052"; };
      };
      forAllSystems = nixpkgs.lib.genAttrs (builtins.attrNames targets);
    in {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          t = targets.${system};
        in {
          default = pkgs.stdenvNoCC.mkDerivation {
            pname = "diffler";
            inherit version;
            src = pkgs.fetchurl {
              url = "${base}/diffler-v${version}-${t.triple}.tar.gz";
              sha256 = t.sha256;
            };
            sourceRoot = ".";
            dontStrip = true;
            installPhase = ''
              install -Dm755 diffler-v${version}-${t.triple}/diffler $out/bin/diffler
            '';
          };
        });
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/diffler";
        };
      });
    };
}
