{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.11.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.11.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "80862b28215a8c05ebf229da0d47a82d49a493f2ea17e421d4a982a86cf0dd0e"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "2eb99ceadea9cc7e08f1dc2f52dd90ac85a347c303878f235b3983a347faf8cf"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "80cc2ff446ea02ad641cf99db6988bd92caf012f57efefd073b315d17d2d3e74"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "9eac100c7caa0dc173d7df8134f818af77fe783d70b43a6d00b02e37cd97974e"; };
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
