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
          overlays = [ fenix.overlays.default ];
        };
        rustToolchain = pkgs.fenix.stable.withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
        ];
        nixosModuleEvalTests = import ./nix/eval-tests.nix {
          inherit nixpkgs system;
        };
        # >>> generated:flake-packages by `cargo generate installers` - do not edit <<<
        # Default feature set: canonical Dist (all channels, no heavyweight).
        # Override with `packages.zeroclaw.override { features = [ ... ]; }`.
        zeroclawDefaultFeatures = [ "acp-bridge" "agent-runtime" "channel-acp-server" "channel-amqp" "channel-bluesky" "channel-clawdtalk" "channel-dingtalk" "channel-discord" "channel-email" "channel-imessage" "channel-irc" "channel-lark" "channel-linq" "channel-mattermost" "channel-mochat" "channel-mqtt" "channel-nextcloud" "channel-notion" "channel-qq" "channel-reddit" "channel-signal" "channel-slack" "channel-telegram" "channel-twitch" "channel-twitter" "channel-voice-call" "channel-wati" "channel-webhook" "channel-wecom" "channel-wecom-ws" "channel-whatsapp-cloud" "gateway" "observability-prometheus" "schema-export" ];
        buildZeroclaw = { pname, cargoPkg, features ? zeroclawDefaultFeatures }:
          (pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          }).buildRustPackage {
            inherit pname;
            version = "0.8.2";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags =
              [ "-p" cargoPkg "--no-default-features" ]
              ++ pkgs.lib.optionals (features != [])
                [ "--features" (pkgs.lib.concatStringsSep "," features) ];
            doCheck = false;
          };
        # >>> end generated:flake-packages <<<
      in {
        packages.zeroclaw = buildZeroclaw { pname = "zeroclaw"; cargoPkg = "zeroclawlabs"; };
        packages.zerocode = buildZeroclaw { pname = "zerocode"; cargoPkg = "zerocode"; };
        packages.default = buildZeroclaw { pname = "zeroclaw"; cargoPkg = "zeroclawlabs"; };
        checks = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          nixos-module-eval = pkgs.writeText "zeroclaw-nixos-module-eval" (
            builtins.toJSON nixosModuleEvalTests
          );
        };
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.rust-analyzer
          ];
        };
      }) // {
      # The `services.zeroclaw` NixOS module (multi-instance; see nix/module.nix
      # and nix/README.md). Exposed as the default so `nixosModules.default` can
      # be imported directly into a system configuration.
      nixosModules.default = import ./nix/module.nix;

      # Toolchain test systems used to evaluate the dev-shell module on both
      # supported Linux architectures; not a deployment target. The `checks`
      # output evaluates these in CI.
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
