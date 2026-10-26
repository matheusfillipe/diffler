{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.21.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.21.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "cceb3b18da2f15e2fb3b9788bd60013b041f35f3f28f8091c1de2063dd7589ef"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "36e828a2d5e45d23bd1e1ef952f1d2d4d65e00b9d3ce503fa444b61c0f88422e"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "03cf20d2cb4cc34d9836d443805188e3c90e4e0578d2ee9fb952b580210f2cf0"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "285ceb74e081bd55fabf81c7654331aea740a5791387f9646ec547e7d7afa9ff"; };
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
