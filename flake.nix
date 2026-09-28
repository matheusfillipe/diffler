{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.16.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.16.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "48f58506c078e1437c1299c2399fff2ed7b210bbc948867fc032b9f063347752"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "78a27e8ce8edcbd0529ea8ff501a534598849bc84c74c802dd6a522432606e30"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "473af6198e480e7d743b129145be55835aa277d28fa663bc1705fac5a62eb443"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "be431e21871c79ac2d97fa86d1505038986b1a6270d195f771f41c9a93e1cd1d"; };
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
