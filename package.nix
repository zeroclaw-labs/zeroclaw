{
  makeRustPlatform,
  rustToolchain,
  lib,
  zeroclaw-web,
  removeReferencesTo,
}:
let
  rustPlatform = makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
in
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
        ]
        ++ (lib.optionals finalAttrs.doCheck [
          ./tests
          ./test_helpers
        ])
      );
    };
  prePatch = ''
    mkdir web
    ln -s ${zeroclaw-web} web/dist
  '';

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [
    removeReferencesTo
  ];

  # Since tests run in the official pipeline, no need to run them in the Nix sandbox.
  # Can be changed by consumers using `overrideAttrs` on this package.
  doCheck = false;

  # Some dependency causes Nix to detect the Rust toolchain to be a runtime dependency
  # of zeroclaw. This manually removes any reference to the toolchain.
  postFixup = ''
    find "$out" -type f -exec remove-references-to -t ${rustToolchain} '{}' +
  '';
})
