{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.19.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.19.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "7dbdca8f05cda2d037541aa638b6027e011553ecf61c679cc8412b3fe818f733"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "c02e79bdd166b0de9ba25c21e6a2fed227f2acad353e6a6e00acfd2f0d58f97c"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "bc573d2b9ef419a7a19c2cef20eeeb5a4a0cb3effcfa43ce9f450639ac3913d3"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "49a36078270c085ffd64b93cf4d71568d3b5a80f62ff68f3e3c4b202e93bc23a"; };
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
