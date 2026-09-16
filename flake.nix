{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.14.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.14.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "4d363be38e6794f65d4cd72e75b7b4bc3ecb5cef7c227d6b6bf8c562adb87917"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "52c057552ce6c2b504cef3139578ba487375c53e51d85327655f0afa1441849c"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "b66e04728cc3f52da92040b00a0a47b0b5982b17d9bcd70cab26d6eeb89d884c"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "7f5e21437dc29e3a5ab3248bcbf4032688e596ac25a903916c26b1ea20e7d906"; };
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
