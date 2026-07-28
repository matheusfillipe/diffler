{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.5";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.5";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "b3a07f683f3bf350803e3b1778b8d4da29114f67bf7d091badae539f2bf80fc9"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "853821f71eb4171a7bce31d497287b573328d18ed8fca82ff57053abf14ce9d9"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "a7c3fd5e7e2d52dd5713b299f97dc8e7164778fb45ec0267a1b7f70c02a1ea63"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "cfd9307d907e711e0827428ae79d934e0cccd0ab66671ab303208477c0ad49da"; };
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
