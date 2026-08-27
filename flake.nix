{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.13.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.13.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "fb01a92c0bb6a5967342c44c64a4297de4b1a684a62a4404cf77cd1094a35008"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "7fc35bfc70770ee04a52addc7183ce8f9f7f604d3185c1efacd4585aa9987b29"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "cad0ab0a86a85d159f5d955cd1332ac578096ee470ca05b820502e6f3239ee2b"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "932cb6717fba859ad241759d0b734e1eba11e5f4351ff1d62784df932b1cef92"; };
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
