{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.1.13";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.1.13";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "3f0de8b9c44555979993b1a9ab1d4037970001d6f91fb2d6d3ff076f66f6de06"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "8da02c6aee4e68da83f95dced991e2dfabcf4784c576f3105872c4e3150140bf"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "9153927aeeecea00b85f5aa22ab3823d6ff01eb3d51f48eb6fd7bf5ad5a888cb"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "84fb533fab5011d85895f0a4af0de03aaffe7a08ad0af4d4c7ea7d9d9f4e8360"; };
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
