{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.9.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.9.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "d7f6f7af3d5754dd46abea74640d8219c17e149362eec1f0d259dea8151f252f"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "42e0666eeef9b6057c7891aee0e045577807368d3d0c9f1720ca3d49fdbdb8fc"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "c454487c7e160d58a33e2b4c9856599b3db0e00e135b617c46d93fb131bdc1aa"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "9197016d6d0d665c9f1be93e89557d0a25845236a0c83e1d21fd333d5500d83e"; };
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
