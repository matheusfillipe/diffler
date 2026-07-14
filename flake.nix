{
  description = "Terminal code review for AI coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      version = "0.5.1";
      base = "https://github.com/matheusfillipe/diffler/releases/download/v0.5.1";
      targets = {
        x86_64-linux = { triple = "x86_64-unknown-linux-musl"; sha256 = "d24ad5673b13f19f1dcc847f715a25b1c226ef1137557ce9105d35479307f254"; };
        aarch64-linux = { triple = "aarch64-unknown-linux-musl"; sha256 = "4c866ffa9b376db88b07e7fe20e771cbcc47bedec74488d621dc923f76ffc5dd"; };
        x86_64-darwin = { triple = "x86_64-apple-darwin"; sha256 = "d116acc1683c81782295fb896c3685cf94325430838422b5efa184227388c4fa"; };
        aarch64-darwin = { triple = "aarch64-apple-darwin"; sha256 = "626ca764b0a541d1873c9f33c5a1e086958569b089881c54a10b4483a9bb82fa"; };
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
