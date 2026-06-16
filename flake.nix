{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.1.12";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.1.12";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "eb5ab4ee0c89e2c3f01a567a5ccfb06241611041b501e4cbedc177f25580b649"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "226d32721ba3bd38d69e8d587a108af9107f3e87fc0811d54d4c9ef9bc7ddd74"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "bf2c68e82765f45fa7a475c0262d98d82aa7be6c294c7d29be867813582402b2"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "da562d7f592f3a75f4de5ec6733acf2cc9a814d27e4d6b17714b2dbd9845bc18"; };
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
