{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.4.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.4.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "a5a06d1f3a7f7e68f4ef88b6c11b26acc9666647b218298a156764f7af7a8177"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "de84fb56827d2834891bf1e8c336f51e5422cd9c832435694ce6e5afc70bed2d"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "fbd877dbd9468f018a0817fa6290bdc5337b9e68e6ce6d221ea075997b3fe40c"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "2b62b3633a6cbbd525e982e8b60bdbdd2dfb03cc78c31ac29dde42809f1569c4"; };
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
