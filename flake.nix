{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.7.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.7.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "8fc3d8156a3c393b57084e029f2cee42d04123b7f37e4a298284130fe78a44bf"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "53a7ba97201da3a1513652c07776a8080599881dfb46ddc5506f6cd6cb0a3c78"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "46c33b41872912c7b5d6640c91024094c60a510bf0464e060966fc0847c41b46"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "9b25a3671ff9294129e590ca8be1faa1181f627e832401fb04945b3c96a0569f"; };
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
