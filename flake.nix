{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.6";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.6";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "c8aa8f7ac29c2e5583d252555a97c808defb22674e7df9ede21f7ff9435f6e8f"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "4f2de2e6b283cfe0e74771f16989fb3b155849499fa22ac8f24e62aac013e0e4"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "eff84e468b80eaded97a4c88f7b56e0217b63e6b474b87bd56dff7e710b5efc4"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "584e7b98e7249da2ba89dc7647892ec0d815613420b985fe41e91c629c4a223c"; };
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
