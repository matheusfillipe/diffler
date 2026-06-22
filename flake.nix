{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.2.3";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.2.3";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "511e574dc23200d3295012ce9035e27733d496dfcbb9a6b77770762e8e782af1"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "defffe115a696b5babfa9de66241aa87f3a1e2d13cc16ef554064f17d9894f75"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "75350b993020a88168ac945eddfa666e7535b1408e3eea9cf8b444ceae13ec9f"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "a69eb1082e1c6e4aad6c22e2d681afef818e01b10b3e56903c9cce53355d2feb"; };
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
