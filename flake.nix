{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.2.2";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.2.2";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "ceaf01198af424ef6e12b87f015fbf05cd0faf0b596ccfa56bce35dad1756e85"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "2984eedfc35afba59535dea86c4924b7e148975e9d36b8602d0ea98112d7a170"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "c963e7361bad5dac6d6a05b29621fc22545900fe4e1b97870f87c925f1e105e6"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "50c48af61ccd1f7c4292065a9a271daceacec3068d0ae2bb7224d2d760b8bc1b"; };
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
