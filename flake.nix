{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.2";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.2";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "a9ef45215670fab55d608443bda21cfccd417dc4c95a6874a950125095e2bcf8"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "7ab0196522e832720696db5b802bd1321152d8b94f62c5a67741c6a46fed2b7b"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "14b858358a8e8e78f9f796e7eb35950177a56a26b0acdd4fe81ba92636282cbc"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "51f4ec1c35d0768646ae144d61de27b5072da9f310e231f788f16e34e37662cd"; };
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
