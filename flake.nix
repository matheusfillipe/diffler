{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.2.7";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.2.7";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "11645abda6d6e5157bb5b55716a20d4b58c27694b51712b2cd2695d88aea6174"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "10ef2d484ee28007a4ebde17eb01096454c8fbca6aadd79c139dd9b9d06da857"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "73cc74ed8777cc710357d8af0c8e6799c191cbca038757f5cbd0a2fceff05605"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "4fe4d9072e9bd45e19892e22c66a5ca5ad60d513e91779fe047f97793cfa23e8"; };
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
