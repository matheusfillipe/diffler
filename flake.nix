{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.23.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.23.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "67ad517e83cfa7c4a4f352f2dccedd115fb6ea2577910c6dd231519942a04874"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "103528e72a2036be6f7b29bc454bee8cac9bb5d50b7f39bf2c0890caf7d5a634"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "87bc27a3d5386e6add7fc7263246c41c60753eccdb53a4b8edfa5686e010bc96"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "5b38f5ef6db82730a26e33f73fea58af621a9466c6168c464b4af4032119008f"; };
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
