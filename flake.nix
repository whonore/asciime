rec {
  description = "An ASCII art webcam filter.";

  inputs.nixpkgs.url = "github:nixos/nixpkgs";

  outputs = {
    self,
    nixpkgs,
  }: let
    pkgs = nixpkgs.legacyPackages.x86_64-linux;
  in {
    packages.x86_64-linux.default = pkgs.rustPlatform.buildRustPackage {
      pname = "asciime";
      version = "0.1.0";
      src = pkgs.lib.cleanSource ./.;
      cargoSha256 = "sha256-/BD3niNFGzPSc8Z3fye+vSrUpVM49qCmB/Fl4TFnz9o=";

      nativeBuildInputs = [pkgs.rustPlatform.bindgenHook];

      meta = with pkgs.lib; {
        inherit description;
        homepage = "https://github.com/whonore/asciime";
        license = with licenses; [mit];
        maintainers = with maintainers; [whonore];
        platforms = platforms.linux;
      };
    };
  };
}
