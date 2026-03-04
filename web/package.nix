{ buildNpmPackage, lib }:
buildNpmPackage {
  pname = "zeroclaw-web";
  version = "0.1.0";

  src =
    let
      fs = lib.fileset;
    in
    fs.toSource {
      root = ./.;
      fileset = fs.unions [
        ./src
        ./index.html
        ./package.json
        ./package-lock.json
        ./tsconfig.json
        ./tsconfig.app.json
        ./tsconfig.node.json
        ./vite.config.ts
      ];
    };

  npmDepsHash = "sha256-H3extDaq4DgNYTUcw57gqwVWc3aPCWjIJEVYRMzdFdM=";

  installPhase = ''
    runHook preInstall
    cp -r dist $out
    runHook postInstall
  '';
}
