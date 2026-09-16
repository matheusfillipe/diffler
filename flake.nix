{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.14.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.14.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "d2a9a9f69943ded2edad5115939da1682ff06e7c726ec9ce328dab572d5220cf"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "cfc3836528a20f2e70330a542bf9a9b7ca8c1e36465ac65383d8726422dfb801"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "4cc767aa95c5c453f64be0dba3baaaae73e86e4cc8066b75664e183c4291c2f2"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "84774fc6b6e2fbb3db2b48b5e3d81bb686f71bca400dd5aa6d08fc5938b40f93"; };
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
