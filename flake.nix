{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.17.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.17.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "de4a36e7bc2208a8cd71ff97ef6dfd25e01ff6dac5dbe4c8b49102951bc6c87f"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "f90d886cbcb342333b3cf871b13c1ef149e0d759b562a0dcdc55a4e956614d89"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "075d6a19ba142fda1aff441f01b4717b8450093cd251fea2ac61cd989308b05e"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "3380943ec826092beeeeeb04615641f329c24f1fee9c97b3d0d11f70c360bafc"; };
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
