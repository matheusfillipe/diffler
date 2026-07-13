{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.4.2";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.4.2";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "fb4418e70d7404009725845816201117a7dcb79a52a3fb599709fed2e31bab68"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "6345a2bd43d107239f0890ed0faa4eb5b19aac019aa36d2c8ab873eec79cf3dc"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "734cb4cfd763f0cf2dbe3eb3d61cf1afe83d2e05e0a93891f470f8471aec31e1"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "1386df847e5fb788ffb23b4620d3894fe304a77c6e26466876aa6c004b906614"; };
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
