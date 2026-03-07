{
  description = "hyperlinked";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }: {
    packages = {
      "x86_64-linux" = let
        pkgs = nixpkgs.legacyPackages.x86_64-linux;
        pkg = pkgs.stdenv.mkDerivation {
          pname = "hyperlinked";
          version = "0.2.4";
          src = pkgs.fetchurl {
            url = "https://github.com/nakajima/hyperlinked/releases/download/v0.2.4/hyperlinked-0.2.4-x86_64-unknown-linux-gnu.tar.gz";
            sha256 = "9fb3ca358f1210a12c87710ecc512deac4081ddf632ee55703943c55b174a6a1";
          };
          sourceRoot = ".";
          installPhase = ''
            install -m755 -D hyperlinked $out/bin/hyperlinked
          '';
        };
      in { hyperlinked = pkg; default = pkg; };
      "aarch64-linux" = let
        pkgs = nixpkgs.legacyPackages.aarch64-linux;
        pkg = pkgs.stdenv.mkDerivation {
          pname = "hyperlinked";
          version = "0.2.4";
          src = pkgs.fetchurl {
            url = "https://github.com/nakajima/hyperlinked/releases/download/v0.2.4/hyperlinked-0.2.4-aarch64-unknown-linux-gnu.tar.gz";
            sha256 = "3e6e4aca709b8d80f7ae3e92b39ad51a7e6a23db00975ec706937515954459d7";
          };
          sourceRoot = ".";
          installPhase = ''
            install -m755 -D hyperlinked $out/bin/hyperlinked
          '';
        };
      in { hyperlinked = pkg; default = pkg; };
    };
  };
}
