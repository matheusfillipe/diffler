{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.23.2";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.23.2";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "1d0e33cd43a0c76797b1ab204c3626562ed9cf3e2f9981ecb6973691881895b7"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "e891f617109c601ff9d06394ff84ab91f1cad87935a53cf97acd8d5360c591fa"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "bf2088c2731ed919dda97e0d54673abc594ac312752ddd311627c0c087dbdf31"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "3e462ab68bc3f971dd345eccf868dcc838e58ca126231a0c2cd1954d050529c9"; };
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
