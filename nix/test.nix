# NixOS test for `services.zeroclaw.instances.<name>`.
#
# Run via the standard nixosTest entry point:
#
#   nix-build -E '
#     (import <nixpkgs/nixos/lib/testing-python.nix> { })
#       .makeTest (import ./nix/test.nix { })
#   '
#
# Or wire into a flake's `checks.${system}` block via
# `pkgs.testers.runNixOSTest`. Either entry point requires KVM on the
# builder.
#
# Asserts:
#   1. Two instances declared in `services.zeroclaw.instances` produce two
#      `zeroclaw-<name>.service` units that both reach `active` within 30 s.
#   2. Each instance has its own state directory under `/var/lib/zeroclaw-<name>`,
#      owned by its own per-instance system user.
#   3. The two per-instance UIDs are distinct (multi-instance isolation).
#   4. `${dataDir}/config.toml` exists, mode 0600, owned by the per-instance
#      user, and round-trips through a TOML parser to the input `settings`.
#   5. The unit's effective hardening profile mentions `ProtectSystem=strict`
#      (sanity check that the module's defaults actually applied).
#
# A no-op stub binary stands in for the real `zeroclaw daemon` so the test
# does not depend on a working ZeroClaw build. The stub validates everything
# we need from the *module*: unit generation, file rendering, user creation,
# hardening defaults.
{
  pkgs ? import <nixpkgs> { },
}:

let
  # Stub `zeroclaw` binary: ignore arguments, sleep forever so systemd's
  # Type=simple treats the unit as active.
  zeroclawStub = pkgs.writeShellApplication {
    name = "zeroclaw";
    text = ''
      # Ignore the daemon argument; just stay alive.
      exec sleep infinity
    '';
  };

  # Wrap the script so `${cfg.package}/bin/zeroclaw` resolves to it, and so
  # `lib.getExe` (which reads `meta.mainProgram`) finds a single binary.
  stubPackage =
    pkgs.runCommand "zeroclaw-stub"
      {
        meta.mainProgram = "zeroclaw";
      }
      ''
        mkdir -p $out/bin
        cp ${zeroclawStub}/bin/zeroclaw $out/bin/zeroclaw
      '';

  moduleUnderTest = ./module.nix;

in
{
  name = "zeroclaw-module";

  nodes.machine =
    { config, pkgs, ... }:
    {
      imports = [ moduleUnderTest ];

      services.zeroclaw.instances.test = {
        package = stubPackage;
        settings = {
          default_provider = "anthropic";
          default_model = "claude-sonnet-4-6";
          default_temperature = 0.4;
          channels.telegram = {
            enabled = true;
            bot_token = "fake-token-for-test";
            allowed_users = [ "12345" ];
          };
        };
      };

      services.zeroclaw.instances.other = {
        package = stubPackage;
        settings = {
          default_provider = "anthropic";
          default_model = "claude-haiku-4-6";
        };
      };

      # `yq-go -p toml` parses the rendered TOML for the round-trip check.
      environment.systemPackages = [
        pkgs.yq-go
        pkgs.coreutils
      ];
    };

  testScript = ''
    machine.start()

    with subtest("both instances start within 30 s"):
        machine.wait_for_unit("zeroclaw-test.service", timeout=30)
        machine.wait_for_unit("zeroclaw-other.service", timeout=30)

    with subtest("each instance has its own dataDir owned by its own user"):
        machine.succeed("test -d /var/lib/zeroclaw-test")
        machine.succeed("test -d /var/lib/zeroclaw-other")
        owner_test = machine.succeed("stat -c '%U' /var/lib/zeroclaw-test").strip()
        owner_other = machine.succeed("stat -c '%U' /var/lib/zeroclaw-other").strip()
        assert owner_test == "zeroclaw-test", f"expected zeroclaw-test, got {owner_test}"
        assert owner_other == "zeroclaw-other", f"expected zeroclaw-other, got {owner_other}"

    with subtest("UIDs are distinct (multi-instance isolation)"):
        uid_test = machine.succeed("id -u zeroclaw-test").strip()
        uid_other = machine.succeed("id -u zeroclaw-other").strip()
        assert uid_test != uid_other, f"both instances share UID {uid_test}"

    with subtest("config.toml exists with mode 0600 and correct owner"):
        machine.succeed("test -f /var/lib/zeroclaw-test/config.toml")
        mode = machine.succeed("stat -c '%a' /var/lib/zeroclaw-test/config.toml").strip()
        owner = machine.succeed("stat -c '%U:%G' /var/lib/zeroclaw-test/config.toml").strip()
        assert mode == "600", f"expected 600, got {mode}"
        assert owner == "zeroclaw-test:zeroclaw-test", f"unexpected owner {owner}"

    with subtest("rendered TOML round-trips through a parser"):
        model = machine.succeed(
            "yq-go -p toml -o json '.default_model' /var/lib/zeroclaw-test/config.toml"
        ).strip().strip('"')
        assert model == "claude-sonnet-4-6", f"expected claude-sonnet-4-6, got {model}"

        other_model = machine.succeed(
            "yq-go -p toml -o json '.default_model' /var/lib/zeroclaw-other/config.toml"
        ).strip().strip('"')
        assert other_model == "claude-haiku-4-6", f"expected claude-haiku-4-6, got {other_model}"

    with subtest("hardening defaults applied (ProtectSystem=strict)"):
        out = machine.succeed("systemd-analyze security zeroclaw-test.service")
        assert "ProtectSystem=strict" in out or "Protect system" in out, (
            f"hardening defaults not applied: {out}"
        )
  '';
}
