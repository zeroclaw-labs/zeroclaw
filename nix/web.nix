# ZeroClaw web dashboard as Nix packages (Option A: split derivations).
#
# - `zeroclaw-openapi-spec`: pure-Rust hermetic dump of the gateway OpenAPI
#   spec plus the Rust-rendered TS helpers. Single source of truth stays
#   `zeroclaw_gateway::openapi::build_spec()`; `cargo xtask web spec-dump`
#   writes `openapi.json`, `api-descriptions.ts`, `api-enums.ts` with no npm.
# - `zeroclaw-web`: `buildNpmPackage` dashboard bundle. `preBuild` derives
#   `api-generated.ts` from the spec via the already-vendored
#   `openapi-typescript`, then the default `npm run build`
#   (`check:generated + tsc -b + vite build`) produces `dist/`.
#   `installPhase` ships only the bundle at
#   `$out/share/zeroclaw-web/` so `gateway.web_dist_dir` can point at it
#   (see `crates/zeroclaw-gateway/src/static_files.rs`).
{
  lib,
  stdenv,
  makeRustPlatform,
  buildNpmPackage,
  importNpmLock,
  nodejs_24,
  runCommand,
  rustToolchain,
}:

let
  # Keep in sync with the workspace version (`Cargo.toml [workspace.package]`)
  # and the Rust packages in `flake.nix`.
  version = "0.8.5";

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes = builtins.fromJSON (builtins.readFile ./hashes.json);
  };

  rustPlatform = makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };

  # Self-contained `cargo xtask web` binary. Only the xtask package is built,
  # but the full workspace lockfile is vendored so the sandbox needs no network.
  xtaskWeb = rustPlatform.buildRustPackage {
    pname = "zeroclaw-xtask-web";
    inherit version;
    src = ../.;
    inherit cargoLock;
    cargoBuildFlags = [
      "-p"
      "xtask"
      "--bin"
      "web"
    ];
    doCheck = false;
    buildInputs = [ stdenv.cc.cc ];
  };

  openapiSpec = runCommand "zeroclaw-openapi-spec" { } ''
    mkdir -p $out
    ${xtaskWeb}/bin/web spec-dump --out $out
  '';
in
{
  inherit openapiSpec;

  zeroclawWeb = buildNpmPackage {
    pname = "zeroclaw-web";
    version = "0.1.0";
    src = lib.cleanSourceWith {
      src = ../web;
      # `node_modules/` (dev installs) and `dist/` (vite output, gitignored)
      # must not enter the store; the bundle is rebuilt hermetically below.
      filter =
        path: _type:
        let
          base = baseNameOf path;
        in
        base != "node_modules" && base != "dist" && base != "result";
    };
    nodejs = nodejs_24;
    npmDeps = importNpmLock {
      npmRoot = ../web;
    };
    # Mandatory pairing: the default `npmConfigHook` byte-compares
    # `src/package-lock.json` against the (re-serialized) lockfile in
    # `npmDeps` and always fails for `importNpmLock` outputs.
    npmConfigHook = importNpmLock.npmConfigHook;

    preBuild = ''
      cp ${openapiSpec}/api-descriptions.ts src/lib/api-descriptions.ts
      cp ${openapiSpec}/api-enums.ts src/lib/api-enums.ts
      npx --no-install openapi-typescript ${openapiSpec}/openapi.json \
        -o src/lib/api-generated.ts
    '';

    # Skip the default npm-pack install hook: ship only the static bundle.
    installPhase = ''
      runHook preInstall
      mkdir -p $out/share/zeroclaw-web
      cp -r dist/. $out/share/zeroclaw-web/
      runHook postInstall
    '';
  };
}
