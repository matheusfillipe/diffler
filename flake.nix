{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.8";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.8";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "1cd8844cf393481330889dccc04696dfbf074d1882c5f1f1a8f2b6e2e0bb1483"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "61bb0001241b0aaf77132336a880871ce8ec5225bcdbb5bd9aa91b43302bb645"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "449d476082ca7f53671554f36de61116a56a918da233afa0bbd9e08b8814410b"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "e721905f940c5ff4b938f9c400134e8216bfcd9597b3d70d8821366cb7692126"; };
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
