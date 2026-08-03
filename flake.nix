{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.8.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.8.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "57677c4693631ee67dd57e529702e936cd7cd7e4f8407d90556aec7d7d3b65e7"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "bc4df88eade0372a0baf47dd46fcfe66835e00c354474cb1e6e0c3368a607a7e"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "dcd5b4bd153f16a24ae16cab8d4b562349dda5a54d7fd59f9fc1eaf25057b38a"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "f31a5d492979a01afa0a401da8c74a958a0f12273ccc8908f0a6a6a377ba3a34"; };
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
