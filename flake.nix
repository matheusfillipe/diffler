{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.13.2";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.13.2";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "062caff4229e5f17efe3d4c6c33c80fd810ccfa95e246134f7fd454d810c9943"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "8f4566240ac2d3b69c98b256bfbc6790c39b93245973fbbf6a0ea14aed602206"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "9f24de08275af2dcfd0d05d174b97568b95b19b22bc868c528f76edd5bfcdcb6"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "5e56df588e1573fb591e98ca3c69b16d62b8df8cccd039d26588e30ecaaee394"; };
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
