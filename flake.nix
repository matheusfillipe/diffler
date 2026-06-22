{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.2.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.2.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "9f8946e1443d2cc7640a412262a1071348d5dceec4e912c60671d6432db53959"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "28e7e8947206f59126d2d9a0db8d7a3a6782e4d206b25c61a67023ae0db75e21"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "b022366ba180b055773a2200cc0ef10589c016a954e70ab45cd6822010429afd"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "69c32579c04c360251bb66ab3465689990b990331bba31e2433b076aba191254"; };
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
