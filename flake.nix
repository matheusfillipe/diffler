{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.1.10";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.1.10";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "f1e85be08c57102efafdd2edaa6d9c18cec0f8d2946d8af8fe638ab0a4a829fa"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "4bf95c1ff12367f0f7129c656561f6b61764bb39f1bb76cf28619a137acb3b73"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "37e944620a8f27a7e2d35f0a006c68e6ab1a4f043a012b4237cfd948ae8b294b"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "133340c8000bb1ccff5d20e1c500d89aea68946241833485b9068da0cdaada54"; };
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
