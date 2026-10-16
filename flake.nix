{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.20.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.20.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "4610b774cee413e956123a567d633c6611a8d423383da2004a34ffc492f92502"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "b79a2a95ddadadeadf4b11c06867a53dbab0cbe05f407fda8bd4be407d6d2136"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "138afa16653ffbb20a8a7a767d1969ec5de01f1b6d613d2c84b391f58964e7a5"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "3fb048d60f7fba054fff29a15653f0867b1b0cc53fdc8a7bb525c358402b328b"; };
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
