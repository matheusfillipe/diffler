{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.9";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.9";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "b50ea0058e8ccbfd7a8e2da43379ca82c9504196b5c2b51455ef98cca3ae6538"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "65cd19ee6416be5c16ec71c4c1c6edd5129686e4dbf9a0180092d7d3e69e2170"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "5019e0d897f0651ad68b39220cae0de2ce2e07ccff178ac0fd65f5c53a4ccc74"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "bbf3546e132b2c9faa3f5be111940799e054ca3cd8e5e739cd2cd30a86ef6878"; };
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
