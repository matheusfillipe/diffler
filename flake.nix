{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.11.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.11.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "b7abb61a1385245249272c143d103266dc4c1de7989987b05af9983a4dbb45f7"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "13bc41c9976ea1f129ad156b9f7e9c1f9ffd7010c05e8c28b62cc7d51601e618"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "57e75c106da88f3839d6114ad325cb1f79dae524e7a01dbf15c07a989abb21d7"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "4c1878ca04b3da5b18d28add69bf6f1e61d4ad2f77ef570d744a069e9023f377"; };
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
