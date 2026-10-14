{
  nixpkgs ? <nixpkgs>,
  system ? "x86_64-linux",
}:

let
  pkgs = import nixpkgs { inherit system; };
  lib = pkgs.lib;

  stubPackage =
    pkgs.runCommand "zeroclaw-eval-stub"
      {
        meta.mainProgram = "zeroclaw";
      }
      ''
        mkdir -p $out/bin
        cat > $out/bin/zeroclaw <<'EOF'
        #!${pkgs.runtimeShell}
        exit 0
        EOF
        chmod +x $out/bin/zeroclaw
      '';

  # Minimal stand-in for `packages.zeroclaw-web`: only the bundle layout
  # (`share/zeroclaw-web/index.html`) matters to the module, which references
  # it via `gateway.web_dist_dir`.
  stubWebPackage = pkgs.runCommand "zeroclaw-web-eval-stub" { } ''
    mkdir -p $out/share/zeroclaw-web
    echo '<html></html>' > $out/share/zeroclaw-web/index.html
  '';

  mkInstance =
    attrs:
    {
      package = stubPackage;
      settings.default_provider = "anthropic";
    }
    // attrs;

  evalConfig =
    instances:
    (import "${nixpkgs}/nixos/lib/eval-config.nix" {
      inherit system;
      modules = [
        ./module.nix
        {
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          system.stateVersion = "26.05";
        }
        {
          services.zeroclaw.instances = instances;
        }
      ];
    }).config;

  failedAssertions =
    instances:
    builtins.filter (assertion: !assertion.assertion) (evalConfig instances).assertions;

  assertionMessages =
    assertions:
    lib.concatStringsSep "\n" (map (assertion: assertion.message) assertions);

  assertPasses =
    name: instances:
    let
      failed = failedAssertions instances;
    in
    if failed == [ ] then
      true
    else
      throw "${name}: expected module eval to pass, but assertion(s) failed: ${assertionMessages failed}";

  assertFailsWith =
    name: expectedSubstring: instances:
    let
      failed = failedAssertions instances;
      matched = lib.any (assertion: lib.hasInfix expectedSubstring assertion.message) failed;
    in
    if matched then
      true
    else
      throw "${name}: expected assertion containing `${expectedSubstring}`, got: ${assertionMessages failed}";
in
{
  duplicateCreatedUsersFail = assertFailsWith "duplicate created users" "same `user` while also setting `createUser = true`" {
    first = mkInstance {
      user = "zeroclaw-shared";
    };
    second = mkInstance {
      user = "zeroclaw-shared";
    };
  };

  sharedUserWithSingleCreatorPasses = assertPasses "shared user with one creator" {
    owner = mkInstance { };
    shared = mkInstance {
      user = "zeroclaw-owner";
      group = "zeroclaw-owner";
      createUser = false;
      dataDir = "/var/lib/zeroclaw-shared";
    };
  };

  distinctCreatedUsersMayShareGroup = assertPasses "distinct created users sharing group" {
    first = mkInstance {
      group = "zeroclaw-shared-group";
    };
    second = mkInstance {
      group = "zeroclaw-shared-group";
    };
  };

  # Without an overlay providing `zeroclaw-web`, plain nixpkgs evaluates the
  # `webUiPackage` default to null (API-only) instead of failing.
  webUiDefaultsToNullWithoutOverlay =
    let
      cfg = evalConfig {
        only = mkInstance { };
      };
    in
    if cfg.services.zeroclaw.instances.only.webUiPackage == null then
      true
    else
      throw "webUiPackage: expected null default without overlay";

  # An explicit bundle evaluates (exercises the `gateway.web_dist_dir`
  # default merge through `tomlFormat.generate`).
  explicitWebUiPackagePasses = assertPasses "explicit web UI package" {
    web = mkInstance {
      webUiPackage = stubWebPackage;
    };
  };

  # An explicit `settings.gateway.web_dist_dir` wins over the bundle default.
  explicitWebDistDirPasses = assertPasses "explicit web_dist_dir wins" {
    web = mkInstance {
      webUiPackage = stubWebPackage;
      settings.gateway.web_dist_dir = "/srv/custom-dist";
    };
  };

  # A raw `[gateway]` table in `extraConfig` merges with the bundle's
  # injected `gateway.web_dist_dir` into a single valid table.
  # Smoke-only: eval passes. Merged content
  # (`port` + `web_dist_dir` coexist, extraConfig wins) is asserted in
  # `nix/test.nix`, which can read the built output.
  gatewayExtraConfigWithBundlePasses = assertPasses "gateway extraConfig with bundle" {
    web = mkInstance {
      webUiPackage = stubWebPackage;
      extraConfig = ''
        [gateway]
        port = 42618
      '';
    };
  };

}
