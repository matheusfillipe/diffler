{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.6.7";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.6.7";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "de99dbb91827519e4ea5e9599c14d6e9ced1dfeebc9b0fb249fda9e3db0b2687"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "58ee18b63e7d6e8d86ddf9da1e2d27af9d2f6437681b5d87dde740f8463627ea"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "b8227c22782ac6930d78ea15fc405e6fe5c5a1ec981f549496187e9a9a42e0a9"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "9fb68192564a3d05bbc63a2bcc7b98ea13858e4197e233e34938c63a332b6fa1"; };
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
