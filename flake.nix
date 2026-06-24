{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.2.5";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.2.5";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "a46ce984e891a8f62d745b6dd56cd8eab13011c7cfcafd033bb1c9e0643e9236"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "025cfeedee334948e9699a1660656c7c85b107b125eafbf028094c62cdb22301"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "91370eea7a1047a56000540d482cae0ece8cc612876c24833eadbdc1940dc8ae"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "a9eb62a22dfdca380f35c2f08a70f3a9d47fe1e7fec94268f1a57a5119e5e2bc"; };
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
