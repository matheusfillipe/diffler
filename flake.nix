{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.3";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.3";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "bc96989e5235ca4ce7150c3ede5f81bd12d815da2c8184c8138dd882c88707cb"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "0acaed51fc07be937a371f963f31f58bc6009ec4aa29f7a26edfb2cfe6a153bd"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "67fa57e7d2d92e39106159136954fdbc1c0c295ab84ca5ab60e2d998161bbb99"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "6aa7a4a1c0ed6d86263caa513d1b2048c290503502cece30fc3709351c435e16"; };
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
