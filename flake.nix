{
  nixConfig = {
    extra-substituters = [
      "https://nix-community.cachix.org"
    ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixpkgs.url = "nixpkgs/nixos-unstable";
  };

  outputs =
    {
      self,
      flake-utils,
      fenix,
      nixpkgs,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            fenix.overlays.default
            (import ./overlay.nix)
          ];
        };
      in
      {
        formatter = pkgs.nixfmt-tree;

        packages = {
          default = self.packages.${system}.zeroclaw;
          inherit (pkgs)
            zeroclaw
            zeroclaw-web
            ;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ pkgs.zeroclaw ];
          packages = [
            pkgs.rust-analyzer
          ];
        };
      }
    )
    // {
      overlays.default = import ./overlay.nix;

      nixosModules = {
        default = self.nixosModules.zeroclaw;
        zeroclaw =
          { pkgs, ... }:
          {
            imports = [
              (import ./nixos/module.nix)
            ];
            services.zeroclaw.package = self.packages.${pkgs.stdenv.hostPlatform.system}.zeroclaw;
          };
      };
    };
}
