{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.1.8";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.1.8";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "112488dbb2d95cfe04064d9882f893916f3aa9ab2c7f802ded472317cf3d964c"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "1ef980a888374c88431673c4921a2941bb77fe89799d08eb0d24e41c4bf26775"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "dc46a52eb600f04bc0cb5c95d53b1ee8d9e412d8df019de1ae0548b130078357"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "acc4277205ba23f97f2c595874c72277b87b5ff1cbc33a29cf71d5ba5e60ecf9"; };
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
