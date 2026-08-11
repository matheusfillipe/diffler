{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.10.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.10.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "d23b4b64a7ee97cb28eaf188e726321b91fbe7a24816465b267e32e527076ce8"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "94f3805d3b41346ad6660bd7a12646f54205ceaea6630bb85fdbf2b72d98a3be"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "71b5aeada97ec504a346181e8775e7c8a195ddb2ed0d1f3600e50c7a04729167"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "0e77cbb25d5b71a1b29c23941d574bae842511a4701406ae2375361ed3f3e473"; };
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
