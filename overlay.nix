final: prev: {
  zeroclaw-web = final.callPackage ./web/package.nix { };

  zeroclaw = final.callPackage ./package.nix {
    rustToolchain = final.fenix.stable.withComponents [
      "cargo"
      "clippy"
      "rust-src"
      "rustc"
      "rustfmt"
    ];
  };
}
