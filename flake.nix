{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.1.6";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.1.6";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "c881739b75f69c2b79109e30bfb908eb01a2424098998f68084f15f463087659"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "879ee0a8f1578158bef8ead16e90b1a5096b426c5a617b2395c684436dadf32c"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "69bdc249563cd120cb41f2fc8a0c860f94d386238d0bf09e60abe899b14221b6"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "cfe0ddffd9824faf91cb145400baba7964f5eff2881ae6011d24aff5b5010bcb"; };
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
