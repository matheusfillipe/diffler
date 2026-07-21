{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.4";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.4";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "4be5ab92b4b831693ea67a5fc4a11d03a31fdd7e222dd1d3813d05bbef1bca51"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "eeba672fc86905b82a0bd8df1f7e56903fdbfb9544d5d90650f692b1c55e2129"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "ec4f5a65fc920b57d7c4cea39dbe3aba73ed2457e7394ef1079150f5bd9093e8"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "15789c30493fee7ec4bd4e65a17fc2e5284d64a4c1362e1b5bae672661aaf4b7"; };
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
