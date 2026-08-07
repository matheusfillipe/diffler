{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.9.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.9.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "42aac75d9e7ec7524ad513ecf30887427afce97e42042377fa2c07a32f003a7e"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "fec6132d67222e864de3523c7b12a4ae0315548560c772727086a5ab1a81a1d5"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "8f3881f1cd7ebee784778deb1c1b42ce9a481ee7c72701f748205c413b009dc8"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "ff7a6f732d422b10924ba373cae28d346ab048fd2a3c2d6be7420e92061b6e4b"; };
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
