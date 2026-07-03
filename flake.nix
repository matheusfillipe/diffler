{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.3.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.3.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "834520be66e9f166eaf3aaef93dc3e7ac0551e1e0fa9e5cdda6cd35c4008aae6"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "45caded5edcfbb0e5e60868b53722eee15b356bdc8f5c995456e67e95cc5587d"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "d512f3bac96431a39b652468b5e8c3501a960e316751f5264552b33a90267327"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "78c69f95594fd0e5d04461139079a1f0bd38b9723344211248b545c25843f0ad"; };
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
