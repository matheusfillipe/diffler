{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.15.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.15.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "2469199688351c2a801536838ed37f0f533bcd2299982724e2c16c1f17526d45"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "0a4d5e1eb48d6f9c264c13e90a3c51e1d5f9cd0a8897e59ea3e0542c5c781222"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "44047bc75971fe9278cb87bafdec6b539d1db7a15ab987d161634a0366a84c43"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "560a0fcec8822098a978499cce0eb70a4f94e7ecaf17195b0dd2bb466daa2ad4"; };
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
