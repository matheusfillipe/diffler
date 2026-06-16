{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.1.11";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.1.11";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "b7695b6eeb5326c591d330a5fcc9f1da4d90957953d994d5019c480701be1ee6"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "a9a8808aff1c48421443578c9df1f2375ae7bded385852b59ab845c3a8d2b39b"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "abd8b663177bd030f69e4b2451f9c8f48962c37b56b06d327608731c9c01db1a"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "6633c3b05f224be184694b3ee4290ae835efb26a0f15771c444152ef46271ccf"; };
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
