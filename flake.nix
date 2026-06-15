{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.1.9";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.1.9";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "3f9a2bf7080cfd1c588680a3710b59bc25b3010df18638a0159e6b8df75bb3b7"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "4279985b983ff73e775e9b23f5c8b38063caa6f919fb1f55ed9cdb3c3ad1674a"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "6f8d054e09a999415731c2bf4a816a1968c4887404578de08f0c1cc68bc2abdb"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "0e27cb9e62d76ec1d08cbe2d68c266745456483ae5c1b93faab2331c7f676931"; };
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
