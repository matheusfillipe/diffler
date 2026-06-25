{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.2.6";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.2.6";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "eee709c86ad61972a002985256c97d4f4ccc047e7d69d2fd0294efaea5446b51"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "338dee86556a3111a5329b63e5d50e7ae0b1820ee0acc55ee35c7084d51c2243"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "240a4f8478b92e221b2e8e07c667ba19637d12b2547f3a4659f91436e5ddcc26"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "7bc8370acf2bec03be97f07af970e9da537b26c1973e4700629f88c9bf019e68"; };
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
