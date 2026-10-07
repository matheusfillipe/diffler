{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.18.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.18.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "702d0cd19766ae0ca03182126524c3cac00b9276fda221011661d2759cc945bb"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "c409a246349e5344c002bcdc7484a5f2f68a8cf2093d87a56dffe4daaef27bfd"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "5c122e1d14d4fdc00ce133ecc2ae4efdd27dfb274da25da82dc787e2414ef8c2"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "d48d4f9e5bb1ccdaeb841b769554abe3db1662e4d1b8cc40082c7521c0a4758c"; };
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
