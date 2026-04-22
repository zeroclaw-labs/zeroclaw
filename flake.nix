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
        version = (fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

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
          pkgs.darwin.apple_sdk_13_0.frameworks.Security
          pkgs.darwin.apple_sdk_13_0.frameworks.SystemConfiguration
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
          version = version;
          src = webSrc;
          # To update this hash, change it to lib.fakeSha256, run nix build, and copy the actual hash from the error message.
          npmDepsHash = "sha256-DVL9kov8y1Eh3BM2Rpw+KbTDL6/AvT/epknM2X/Gf3E=";

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
          # doCheck is disabled here because ZeroClaw tests often interact with the filesystem or
          # network, which is difficult to sandbox reliably in a Nix build. Use ./dev/ci.sh
          # for full validation.
          doCheck = false;
        };

        zeroclaw = pkgs.runCommand "zeroclaw" {
          nativeBuildInputs = [ pkgs.makeWrapper ];
        } ''
          mkdir -p $out/bin
          makeWrapper ${zeroclawBin}/bin/zeroclaw $out/bin/zeroclaw \
            --set ZEROCLAW_WEB_DIST_DIR_NIX ${zeroclawWeb}/dist
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
