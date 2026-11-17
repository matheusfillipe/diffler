{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.23.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.23.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "cd693d5b428e4cc355304018514908dfaffd586ca0b6959adfd087f2236a1d1a"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "0111977a37c2168388cdeb7f48a097bf51eb14896b022af8e8e0248b03d48c2c"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "14e9ab4d5c5fed73b7d13266138cb6f34c624f97491d2123e88d8df417ba1d90"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "737fee3f6b4a01b1f97893aa67b1d8287466015d6adc576df5aa55a0b5781d90"; };
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
