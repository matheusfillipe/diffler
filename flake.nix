{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.16.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.16.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "4dea835221c33ed95c237bac7f9957cccdfbe76d1ab06a96c51daba0f3d0cff4"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "d16279ac2e3cbc77443a34e49e3e2f8336ca210106db3157a4607dcde27c6c4b"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "d650cf87ca928d795b1be3c7e79a08567ad8de0bd4a121d15ea82e9567322680"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "2ce1ad2360c39470cf7c70468a9af48539353f23ec8fb50e86b16b3c6b11b1ac"; };
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
