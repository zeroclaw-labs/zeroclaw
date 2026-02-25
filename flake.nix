{
  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixpkgs.url = "nixpkgs/nixos-unstable";
  };

  outputs = { flake-utils, fenix, nixpkgs, ... }:
    let
      nixosModule = { pkgs, ... }: {
        nixpkgs.overlays = [ fenix.overlays.default ];
        environment.systemPackages = [
          (pkgs.fenix.stable.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rustc"
            "rustfmt"
          ])
          pkgs.rust-analyzer
        ];
      };
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            fenix.overlays.default
            (import ./overlay.nix)
          ];
        };
        rustToolchain = pkgs.fenix.stable.withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
        ];
      in {
        formatter = pkgs.nixfmt-tree;
        packages = {
          default = self.packages.${system}.zeroclaw;
          inherit (pkgs) zeroclaw;
        };
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.rust-analyzer
          ];
        };
      }) // {
      nixosConfigurations = {
        nixos = nixpkgs.lib.nixosSystem {
          system = "x86_64-linux";
          modules = [ nixosModule ];
        };

        nixos-aarch64 = nixpkgs.lib.nixosSystem {
          system = "aarch64-linux";
          modules = [ nixosModule ];
        };
      };
    };
}
