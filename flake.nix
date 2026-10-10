{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.19.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.19.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "f6144347176f432bd6e28b662d4d272023563d3d156eb88ca196e42fb1d63796"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "f6e37f315a3ac46c7fc4cc9559d13b7d63fc28b8a09a3096cc3b0e47bfd51341"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "eacb162de817186d21edf30b5b42c5a054fb7a85431f46a1a07a0c29262d6f63"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "f64ac45df8368167478658423d8741d02b435d99523465be97736a4c442ec056"; };
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
