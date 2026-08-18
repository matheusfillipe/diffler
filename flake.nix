{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.12.0";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.12.0";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "7caf61cd4d48a6fdb5bcf3b716a5955951000f9774070eeb1e7a740281c960c9"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "676b932f78c1951e646170906d91f3fba3c5cbdbcf663d60e25697c11c686444"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "455de87baf75e33ce8f0a9779061b64ef49c6da5313a8eee32aa069e079d0afc"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "0eb12facb43f1bf1ca7c728836342b81642b725bdb467db1ba6d2abac643e0e7"; };
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
