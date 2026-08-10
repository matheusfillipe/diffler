{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.10.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.10.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "208bd6d0890c6396ef396f126b3935500de107d5e0cfa50731018922237a13d8"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "0a9b0d9698ea4b3ac87ca2ab326d370a681f8c63ac477ba7ad1d94db3f5662be"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "b315ba90aee224cf97e8a7d122042e935744399894391006a2f657a8c17c6465"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "31fc1fe8df5d2f8adbdb15eae2c4efbfcff67baeea1688171dfaba8b5c012b44"; };
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
