{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.2.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.2.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "02ef981daf93efe77219f79abd9941dd5990551eb9b614929826af7c09276227"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "377f076ffd95c2b3ee4d56bafb7c9226ad9c54970b4de7484fb2d3d965e450bd"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "c1d71ef495028da7993d4705f7542b2eaff7212ee92ecdf9dea6a79303758d5c"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "163bc3045fff2a71a51d00510fc6d53d0e9199d7335e8d159a69a55d513c3793"; };
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
