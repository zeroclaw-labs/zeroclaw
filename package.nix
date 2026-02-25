{ rustPlatform, lib }:
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "zeroclaw";
  version = "0.1.7";

  src =
    let
      fs = lib.fileset;
    in
    fs.toSource {
      root = ./.;
      fileset = fs.unions (
        [
          ./src
          ./Cargo.toml
          ./Cargo.lock
          ./crates
          ./benches
          ./web
        ]
        ++ (lib.optionals finalAttrs.doCheck [
          ./tests
          ./test_helpers
        ])
      );
    };

  cargoLock.lockFile = ./Cargo.lock;

  # Since tests run in the official pipeline, no need to run them in the Nix sandbox.
  # Can be changed by consumers using `overrideAttrs` on this package.
  doCheck = false;
})
