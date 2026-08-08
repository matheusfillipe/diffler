{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.9.2";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.9.2";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "89df187f3df153bdbcf50ca96f31ac8f37261cdf5e70fe652b3ece56b4353a55"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "b7f4adb18f6d2ec7f0cc7895488415968dd5f548751598fef7b4cb934faee3ec"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "71c13a6e78527dfe5241f66812903e1dc422c64b869095f61553627d72fcf0f3"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "540f69a4af9ef66e4ddb2560b20918c0762a57389f0be5772bb0506532b0ef41"; };
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
