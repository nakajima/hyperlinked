{
  description = "hyperlinked";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }: {
    packages = {
      "x86_64-linux" = let
        pkgs = nixpkgs.legacyPackages.x86_64-linux;
        pkg = pkgs.stdenv.mkDerivation {
          pname = "hyperlinked";
          version = "0.2.10";
          src = pkgs.fetchurl {
            url = "https://github.com/nakajima/hyperlinked/releases/download/v0.2.10/hyperlinked-0.2.10-x86_64-unknown-linux-gnu.tar.gz";
            sha256 = "cc1ea43178f9b55e79dc065db9ae4a1e5627242969f3c844b8c21f24ecc4122c";
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
          version = "0.2.10";
          src = pkgs.fetchurl {
            url = "https://github.com/nakajima/hyperlinked/releases/download/v0.2.10/hyperlinked-0.2.10-aarch64-unknown-linux-gnu.tar.gz";
            sha256 = "6374b669e33791b94d6ea9b5e6f733f6340a74c1b061f7733dbb61a796685acf";
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
