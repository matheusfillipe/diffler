{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "d0db644e15bd5b1b68efb2904adc4054a0795c94a471ce903ee5382df5371aa9"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "4d7095fbed773b6621a33370f47df8f306d244732fd1eae7bc01746b157f1f5a"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "87ba934ff2f1e6b892683191e3f8dc8d0c2fdd525431bdf14e14843153befde2"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "6cd546762f9f09bc4626806c5db6bb9607d56dae165fb5c491c9e3f44930c5a1"; };
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
