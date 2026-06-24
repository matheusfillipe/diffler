{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.2.4";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.2.4";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "9c2d210d18fc28a1099adcf281c7c37d0a4cca1581e41773c0a5635adacf4973"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "ccdb119d78cf471fc37a3f00e9ec685269745d67d863ab23f0688353005dbda7"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "0570afd9fffa9138a05625afc3d0585f7ca45a6f254f575433c1c8e96b8752be"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "13a1af747d0d0bdc39525fa111f125c4e8a861ddda154af86f9cad5e918671e0"; };
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
