{
  description = "ZeroClaw - Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixpkgs.url = "nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { flake-utils, fenix, nixpkgs, crane, ... }:
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
          overlays = [ fenix.overlays.default ];
        };

        inherit (pkgs) lib;

        rustToolchain = pkgs.fenix.stable.withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
        ];
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        buildInputs = [
          pkgs.openssl
        ]
        ++ lib.optionals pkgs.stdenv.isDarwin [
          pkgs.darwin.apple_sdk.frameworks.Security
          pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
        ];
        nativeBuildInputs = [ pkgs.pkg-config ];

        # Clean source filter for Rust: includes Cargo.lock/toml and src, excludes web and non-build assets
        rustSrc = lib.cleanSourceWith {
          src = pkgs.nix-gitignore.gitignoreSourcePure [
            "/web"
            "/docs"
            "/examples"
            "/tests"
            "flake.nix"
            "flake.lock"
            "/.dockerignore"
            "/.gitignore"
            "/*.md"
          ] ./.;
          filter = craneLib.filterCargoSources;
        };

        # Clean source filter for Web: includes only the web directory, excludes common noise
        webSrc = lib.cleanSourceWith {
          src = pkgs.nix-gitignore.gitignoreSourcePure [
            "/node_modules"
            "/dist"
          ] ./web;
          filter = _path: _type: true;
        };

        cargoArtifacts = craneLib.buildDepsOnly {
          src = rustSrc;
          inherit buildInputs nativeBuildInputs;
        };

        zeroclawWeb = pkgs.buildNpmPackage {
          pname = "zeroclaw-web";
          version = "0.7.3";
          src = webSrc;
          npmDepsHash = "sha256-0AThpojpIjfjLfhfVlBWsJw4e7ksyi0nX4T60PHB2gc=";

          # Use npm install --legacy-peer-deps if needed, or stick to defaults
          npmFlags = [ "--legacy-peer-deps" ];

          buildPhase = ''
            npm run build --if-present
          '';
          installPhase = ''
            mkdir -p $out
            cp -r dist $out/dist
          '';
        };

        zeroclawBin = craneLib.buildPackage {
          src = rustSrc;
          inherit buildInputs nativeBuildInputs cargoArtifacts;

          # Redundant flags removed as they are in Cargo.toml
          doCheck = false;
        };

        # TODO: I tried to use `ln` and `ln -s` instead of `cp`, but it resulted in web ui being not found. Find some other solution.
        zeroclaw = pkgs.runCommand "zeroclaw" {
          nativeBuildInputs = [ pkgs.makeWrapper ];
        } ''
          mkdir -p $out/bin
          # Copy binary so current_exe() doesn't resolve to the isolated zeroclawBin path
          cp ${zeroclawBin}/bin/zeroclaw $out/bin/zeroclaw
          mkdir -p $out/bin/web
          ln -s ${zeroclawWeb}/dist $out/bin/web/dist
          wrapProgram $out/bin/zeroclaw
        '';

      in
      {
        packages.default = zeroclaw;
        packages.zeroclaw = zeroclaw;
        packages.zeroclawWeb = zeroclawWeb;

        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.rust-analyzer
            pkgs.cargo-watch
            pkgs.cargo-edit
          ];
          inherit buildInputs nativeBuildInputs;
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
