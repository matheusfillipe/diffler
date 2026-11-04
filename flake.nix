{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.21.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.21.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "7229057f20703ae2929c77698e3b400f623130f14f2987abd2fec6cb91a976e5"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "2d101d1e0bc1e08413983645589127819e503bf03eb6716c5c4fb0be6f4d3041"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "417109b4d0f41590e22c5f6c04565a8937a240274c7bfc2cb8bf6933fa07bba2"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "3388e532ecabcb00ae8ed5351070468ace336492e940b6e335b3a154b62daa64"; };
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
