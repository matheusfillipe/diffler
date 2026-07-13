{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.5.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.5.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "9ab559d29bcd7ef488394022cb7e87b3f8d04b6a10362d0629b82e8174af1f41"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "b9c1df393eecfe109310eae41c922e45a245562f1b15373d28410d65b50a0b34"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "d21b2eea79fb3122f391b18ab52a7d61ed56bade770274d8befaf1695560d12f"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "fc2c61f8610a60ccb89e673ce8555d7693c218700b42d39d0d2280b8ef7332e9"; };
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
