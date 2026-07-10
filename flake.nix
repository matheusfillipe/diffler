{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.4.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.4.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "163b184a3289c800f6950f63c99298184fd8af4fbc6b4f37cc972bd7ce88b1ee"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "58d1b54763279449047e60d874cdad934206149c72381c1dece1f542d751e87e"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "c8ba337c0203a9e68fe99eac3b408acf2ce50d358aaf0787dc8591202d199dfe"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "3bd7de513db0f099edd41dc18f5c9c19f76cede53c84005fdaed944d13710149"; };
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
