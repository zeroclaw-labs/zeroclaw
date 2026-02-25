final: prev: {
  zeroclaw-web = final.callPackage ./web/package.nix { };

  zeroclaw = final.callPackage ./package.nix {
    rustPlatform =
      let
        rustToolchain = final.fenix.stable.withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
        ];
      in
      final.makeRustPlatform {
        cargo = rustToolchain;
        rustc = rustToolchain;
      };
  };
}
