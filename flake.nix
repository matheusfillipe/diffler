{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.22.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.22.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "5b3f79d04e73af01d4659efb7411dee494fcd9a40280aa387b3c22e91a21cc34"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "ec4502ae76b5a8bc28a2d800f43bc3ab5ea58b6d18c30f46367b135ecedc7e18"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "f4004b57ac7e97a560613cbaa8808daee2f4964d6be1034826d15a93e2e5660f"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "5c8cc037653fc1d9e0c9d2958a4b92438aafc5fdaea98c64394d9264a0e40322"; };
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
