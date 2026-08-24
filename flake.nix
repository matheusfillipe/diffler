{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.13.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.13.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "9804a7009ae197426207063bf0727e2ece03661b7e8cfdac0ecd076f3a0252b2"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "8ded159ff7837a2faac396b4a45b1bc243317b3a6bb5b7b1bc0d22f3820f4fa9"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "b3c4feb01e5563ee600b6eb75d929a5fbf98f7f1697217d32aabba36fd407278"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "b17b518dc6ecbf1b3642d74b3739d6468e909d685efcaf722f4ed737595abe05"; };
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
