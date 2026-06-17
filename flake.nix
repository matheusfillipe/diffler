{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.1.14";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.1.14";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "23209cbf0da129274c3088a9c65ef65d5ffa4eeabd7f1b5540fbfa908e1defd1"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "f48f8187a595cbae899b96b338938fc7504329ab1b649e71f509b386ae88c0cd"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "057a188cd433815c25b878e51ecc59b149b83b6e6d28d7d09476a16e72a2875a"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "98263b6883f1c1f07a01cc08e4538a84fa5025938b7bb000dce52484ed5189e4"; };
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
