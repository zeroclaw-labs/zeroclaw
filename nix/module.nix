# services.zeroclaw — multi-instance NixOS module for the ZeroClaw agent.
#
# Design memo: see ./README.md for usage; the upstream PR body links the full
# design rationale.
#
# Layout:
#   - `services.zeroclaw.instances.<name>` is an attrset of instances.
#     Membership in the attrset is the activation signal — there is no
#     top-level `enable`. Mirrors `services.restic.backups.<name>`.
#   - Each instance gets:
#       * a dedicated systemd unit          `zeroclaw-<name>.service`
#       * a dedicated state directory       `/var/lib/zeroclaw-<name>`
#       * a dedicated system user / group   `zeroclaw-<name>`
#       * a rendered config file            `${dataDir}/config.toml`
#   - `settings` is a TOML-typed attrset rendered via `pkgs.formats.toml`
#     per RFC-42. `extraConfig` (raw TOML) is the documented escape hatch.
#   - Secrets travel through `environmentFile` (systemd `EnvironmentFile=`),
#     never through `settings`. The unit's `ExecStartPre` runs `envsubst`
#     against the rendered TOML so `$VAR` / `${VAR}` references in `settings`
#     strings expand to the values loaded from `environmentFile` at unit
#     start. The world-readable copy in `/nix/store` only ever contains the
#     literal placeholders; the resolved file lives at `${dataDir}/config.toml`
#     mode `0600`, owned by the per-instance user.
#     This substitution is a property of *this module*, not of ZeroClaw —
#     ZeroClaw itself reads `config.toml` verbatim plus a handful of named
#     env-var overrides documented in `crates/zeroclaw-config/src/schema.rs`
#     (e.g. `OPENROUTER_API_KEY`, `ZEROCLAW_PROVIDER`).
#
# Single-instance usage (laptop / single-host case):
#
#   services.zeroclaw.instances.me = {
#     environmentFile = "/run/agenix/zeroclaw-bot-token";
#     settings = {
#       default_provider = "anthropic";
#       default_model = "claude-sonnet-4-6";
#       channels.telegram = {
#         enabled = true;
#         bot_token = "$BOT_TOKEN";   # systemd $VAR — substituted at load
#         allowed_users = [ "12345" ];
#       };
#     };
#   };
#
# Multi-instance usage (one box, N tenants — shape mirrors restic.backups):
#
#   services.zeroclaw.instances = lib.genAttrs slots (n: {
#     environmentFile = "/run/secrets/${n}/identity.env";
#     settings = (import ./shared-settings.nix) { slot = n; };
#   });
{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib)
    types
    mkOption
    mkIf
    mkPackageOption
    mapAttrs'
    mapAttrsToList
    nameValuePair
    filterAttrs
    optionalAttrs
    literalExpression
    ;

  cfg = config.services.zeroclaw;

  # `pkgs.formats.toml` is the canonical RFC-42 shape: it both type-checks
  # the `settings` attrset at evaluation time and serialises it to TOML at
  # build time. Avoids the `builtins.toJSON | replaceStrings` anti-pattern
  # found in some out-of-tree modules (which loses type validation and
  # mishandles values JSON cannot round-trip).
  tomlFormat = pkgs.formats.toml { };

  # Per-instance submodule. `name` is the attrset key — we use it to default
  # user, group, and dataDir so a caller who only sets `settings` gets
  # sensible, collision-free defaults.
  instanceModule =
    { name, ... }:
    {
      options = {
        package = mkPackageOption pkgs "zeroclaw" { };

        user = mkOption {
          type = types.str;
          default = "zeroclaw-${name}";
          defaultText = literalExpression ''"zeroclaw-''${name}"'';
          description = ''
            System user the instance runs as. Created by the module unless
            {option}`createUser` is `false`.
          '';
        };

        group = mkOption {
          type = types.str;
          default = "zeroclaw-${name}";
          defaultText = literalExpression ''"zeroclaw-''${name}"'';
          description = ''
            System group the instance runs as. Created by the module unless
            {option}`createUser` is `false`.
          '';
        };

        createUser = mkOption {
          type = types.bool;
          default = true;
          description = ''
            Whether the module should create the {option}`user` and
            {option}`group`. Set to `false` to bring your own user — for
            example, a shared system user already declared elsewhere.
          '';
        };

        dataDir = mkOption {
          type = types.path;
          default = "/var/lib/zeroclaw-${name}";
          defaultText = literalExpression ''"/var/lib/zeroclaw-''${name}"'';
          description = ''
            State directory. Holds `config.toml`, the workspace at
            `''${dataDir}/workspace`, and ZeroClaw's SQLite databases.

            Created by `systemd-tmpfiles` at activation time with mode `0750`
            owned by {option}`user`:{option}`group`, so any absolute path is
            valid — `/var/lib/zeroclaw-me`, `/srv/zeroclaw-me`, or a nested
            location like `/var/lib/zeroclaw/me` all work and are created
            on a fresh machine before the unit's `ExecStartPre` runs.
          '';
        };

        settings = mkOption {
          type = types.submodule {
            # RFC-42 shape: typed options for the popular knobs go here later
            # once the surface stabilises; `freeformType` lets every other
            # ZeroClaw config key flow through with TOML's value-model
            # validation. No string-of-doom escape hatch needed for the
            # common case.
            freeformType = tomlFormat.type;
          };
          default = { };
          example = literalExpression ''
            {
              default_provider = "anthropic";
              default_model = "claude-sonnet-4-6";
              channels.telegram = {
                enabled = true;
                bot_token = "$BOT_TOKEN";
                allowed_users = [ "12345" ];
              };
            }
          '';
          description = ''
            ZeroClaw configuration as a Nix attrset. Rendered to TOML in the
            Nix store at build time, then `envsubst`'d into
            `''${dataDir}/config.toml` (mode `0600`) by the unit's
            `ExecStartPre`.

            String values may contain `$VAR` or `''${VAR}` references — they
            expand against the environment loaded from
            {option}`environmentFile` at unit start. This is the recommended
            path for secrets: the build-time copy in the world-readable
            `/nix/store` only ever contains the literal placeholders; the
            resolved file in `''${dataDir}/config.toml` is locked to
            {option}`user`:{option}`group` mode `0600`.

            The substitution is performed by this module, not by ZeroClaw.
            ZeroClaw reads `config.toml` verbatim and overlays a handful of
            named environment-variable overrides on top (e.g.
            `OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `ZEROCLAW_PROVIDER`,
            `ZEROCLAW_MODEL`); any other secret-bearing field — Telegram
            `bot_token`, Discord `bot_token`, etc. — needs the
            `envsubst` path to avoid living in `/nix/store`.

            See ZeroClaw's `config.toml.example` upstream for the full key
            surface; only the shape we render here is module-contractual.
          '';
        };

        environmentFile = mkOption {
          type = types.nullOr types.path;
          default = null;
          example = "/run/agenix/zeroclaw-bot-token";
          description = ''
            Path to a file containing `KEY=VALUE` lines, loaded into the
            unit's environment via systemd `EnvironmentFile=` (see
            {manpage}`systemd.exec(5)`). Variables become available for
            `$VAR` substitution in {option}`settings` strings at process
            start.

            Typical: an agenix- or sops-decrypted file at `/run/agenix/...`.

            When this option is set, the unit declares
            `ConditionPathExists=` on the path, so the unit stays inactive
            (rather than failing) until the secret materialises — useful for
            sops-nix / agenix activation timing.
          '';
        };

        extraConfig = mkOption {
          type = types.lines;
          default = "";
          example = ''
            [experimental]
            new_knob = true
          '';
          description = ''
            Raw TOML appended verbatim after the rendered {option}`settings`
            block. Documented escape hatch (per RFC-42) for ZeroClaw config
            keys whose shape isn't yet covered by the typed `settings`
            surface — most things should go through `settings` instead.
          '';
        };

        bindReadOnlyPaths = mkOption {
          type = types.attrsOf types.path;
          default = { };
          example = literalExpression ''
            {
              "/var/lib/zeroclaw-me/workspace/skills/git" = "/etc/zeroclaw-skills/git";
            }
          '';
          description = ''
            Read-only bind-mounts to thread into the unit's namespace via
            systemd `BindReadOnlyPaths=`. Map of `target = source`. Useful
            for declarative skill bundles, CA bundles, or other operator-
            managed read-only assets.
          '';
        };
      };
    };

  # If a caller needs an escape hatch beyond the typed options, the
  # standard NixOS pattern is:
  #
  #   systemd.services."zeroclaw-myinstance".serviceConfig.MemoryMax =
  #     lib.mkForce "1G";
  #
  # We deliberately do NOT expose an `extraServiceConfig` option — it
  # adds a second way to do the same thing (which always invites
  # contradictions) and isn't standard nixpkgs shape.

  # Render config.toml = formats.toml.generate settings (+ optional extraConfig).
  renderConfigFile =
    name: instanceCfg:
    let
      base = tomlFormat.generate "zeroclaw-${name}-config.toml" instanceCfg.settings;
    in
    if instanceCfg.extraConfig == "" then
      base
    else
      pkgs.runCommand "zeroclaw-${name}-config.toml" { } ''
        cat ${base} > $out
        cat <<'ZEROCLAW_EXTRA_CONFIG_EOF' >> $out

        ${instanceCfg.extraConfig}
        ZEROCLAW_EXTRA_CONFIG_EOF
      '';

  # Build one systemd service from one instance entry. Mirrors the shape of
  # `services.restic.backups`'s mapAttrs' generator.
  mkInstanceService =
    name: instanceCfg:
    let
      configFile = renderConfigFile name instanceCfg;

      # Wrap the envsubst step in a `writeShellApplication` rather than a
      # raw `bash -c '…'` ExecStartPre: systemd parses ExecStartPre with
      # shell-style quoting that doesn't tolerate literal newlines inside
      # a single quoted argument, and a multi-line script keeps the
      # readable error-handling around the empty-output guard.
      configResolveScript = pkgs.writeShellApplication {
        name = "zeroclaw-${name}-resolve-config";
        runtimeInputs = [ pkgs.envsubst ];
        text = ''
          set -euo pipefail
          tmp="${instanceCfg.dataDir}/.config.toml.tmp"
          envsubst < ${configFile} > "$tmp"
          if [ ! -s "$tmp" ]; then
            echo "zeroclaw-${name}: rendered config.toml is empty after envsubst" >&2
            rm -f "$tmp"
            exit 1
          fi
          chmod 0600 "$tmp"
          mv -f "$tmp" "${instanceCfg.dataDir}/config.toml"
        '';
      };
    in
    nameValuePair "zeroclaw-${name}" {
      description = "ZeroClaw agent (instance ${name})";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      # Gate startup on the secret file existing, when one is declared.
      # systemd records ConditionPathExists failure as inactive (dead) with
      # a condition-check note rather than a unit failure — the right
      # behaviour for runtime-provisioned secrets.
      unitConfig = optionalAttrs (instanceCfg.environmentFile != null) {
        ConditionPathExists = instanceCfg.environmentFile;
      };

      environment = {
        ZEROCLAW_CONFIG_DIR = instanceCfg.dataDir;
        ZEROCLAW_WORKSPACE = "${instanceCfg.dataDir}/workspace";
      };

      serviceConfig = {
        Type = "simple";
        User = instanceCfg.user;
        Group = instanceCfg.group;

        # Resolve the rendered config from /nix/store into
        # ${dataDir}/config.toml so ZeroClaw reads it at a stable path
        # *and* `$VAR` / `${VAR}` references inside `settings` strings
        # expand against the unit environment (populated by
        # `EnvironmentFile=`). We use a tiny shell wrapper rather than
        # raw `envsubst` so we can fail fast if the substitution leaves
        # the file empty. `dataDir` is created up-front by
        # `systemd.tmpfiles` in the host config block below; the unit's
        # User= owns that directory, so no chown is needed (which is
        # important — our `CapabilityBoundingSet=""` drops CAP_CHOWN).
        ExecStartPre = [ (lib.getExe configResolveScript) ];
        ExecStart = "${lib.getExe instanceCfg.package} daemon";
        WorkingDirectory = instanceCfg.dataDir;
        Restart = "on-failure";
        RestartSec = "5s";
        TimeoutStopSec = "15s";

        # `dataDir` is created by `systemd.tmpfiles.settings` (see the
        # host config block below) so that arbitrary paths — not just
        # the `/var/lib/zeroclaw-<name>` default — are valid. We deliberately
        # don't use `StateDirectory=`: it derives the on-disk path from
        # the unit's basename under `/var/lib/`, so a caller-supplied
        # `dataDir = "/srv/zeroclaw-me"` would create `/var/lib/zeroclaw-me`
        # (wrong) instead of the path the rest of the unit references.

        EnvironmentFile = mkIf (instanceCfg.environmentFile != null) [ instanceCfg.environmentFile ];

        # Hardening defaults — modelled after `services.atticd` in
        # nixpkgs (a comparable Rust server). Tuned conservatively;
        # callers who need to relax a specific knob should do so via
        # `systemd.services."zeroclaw-<name>".serviceConfig.X = mkForce ...`
        # rather than via a module escape hatch.
        NoNewPrivileges = true;
        PrivateTmp = true;
        PrivateDevices = true;
        # Closed device policy + empty allow-list — matches `atticd`.
        # ZeroClaw doesn't need /dev/* nodes for normal operation.
        DeviceAllow = "";
        DevicePolicy = "closed";
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        ProcSubset = "pid";
        # MemoryDenyWriteExecute=yes blocks W+X mappings; safe for a
        # Rust binary with no JIT. ZeroClaw 0.7.x has no JIT path.
        MemoryDenyWriteExecute = true;
        # PrivateUsers=yes runs the unit in its own user namespace. The
        # StateDirectory= bind-mount happens in the host namespace
        # before the userns remap, so file ownership stays correct from
        # the host's view. Matches `atticd`.
        PrivateUsers = true;
        # RemoveIPC=yes wipes any sysvipc/posix IPC objects the unit
        # leaves behind on stop. ZeroClaw doesn't use SysV IPC, so this
        # is essentially a belt-and-braces cleanup.
        RemoveIPC = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        LockPersonality = true;
        SystemCallArchitectures = "native";
        CapabilityBoundingSet = [ "" ];
        AmbientCapabilities = [ "" ];
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
        UMask = "0077";

        ReadWritePaths = [ instanceCfg.dataDir ];

        BindReadOnlyPaths = mkIf (instanceCfg.bindReadOnlyPaths != { }) (
          mapAttrsToList (target: source: "${source}:${target}") instanceCfg.bindReadOnlyPaths
        );
      };
    };

in
{
  options.services.zeroclaw = {
    instances = mkOption {
      type = types.attrsOf (types.submodule instanceModule);
      default = { };
      description = ''
        ZeroClaw instances to run on this host. Each entry produces a
        `zeroclaw-<name>.service` systemd unit with its own state
        directory, system user, and rendered `config.toml`.

        Membership IS the activation signal — there is no top-level
        `enable`. Mirrors `services.restic.backups`. To temporarily
        disable an instance, wrap it in `lib.mkIf condition { ... }` at
        the call site, or remove the entry.
      '';
      example = literalExpression ''
        {
          me = {
            environmentFile = "/run/agenix/zeroclaw-bot-token";
            settings = {
              default_provider = "anthropic";
              default_model = "claude-sonnet-4-6";
              channels.telegram = {
                enabled = true;
                bot_token = "$BOT_TOKEN";
                allowed_users = [ "12345" ];
              };
            };
          };
        }
      '';
    };
  };

  config = mkIf (cfg.instances != { }) {
    # Per-instance users (only those with createUser = true).
    users.users = mapAttrs' (
      name: instanceCfg:
      nameValuePair instanceCfg.user {
        isSystemUser = true;
        group = instanceCfg.group;
        home = instanceCfg.dataDir;
        createHome = false; # dataDir is created by systemd-tmpfiles.
        description = "ZeroClaw instance ${name}";
      }
    ) (filterAttrs (_: i: i.createUser) cfg.instances);

    users.groups = mapAttrs' (_: instanceCfg: nameValuePair instanceCfg.group { }) (
      filterAttrs (_: i: i.createUser) cfg.instances
    );

    # One systemd unit per instance.
    systemd.services = mapAttrs' mkInstanceService cfg.instances;

    # Create each instance's dataDir at activation time, owned by the
    # per-instance user/group, mode 0750. We do this via systemd-tmpfiles
    # (rather than systemd's `StateDirectory=`) because `StateDirectory=`
    # forces the directory under `/var/lib/<basename>`; a caller who sets
    # `dataDir = "/srv/zeroclaw-me"` would otherwise see `/var/lib/zeroclaw-me`
    # created and the unit would then fail at `WorkingDirectory=/srv/...`.
    # tmpfiles handles arbitrary absolute paths uniformly.
    systemd.tmpfiles.settings."10-zeroclaw" = mapAttrs' (
      _: instanceCfg:
      nameValuePair instanceCfg.dataDir {
        d = {
          mode = "0750";
          user = instanceCfg.user;
          group = instanceCfg.group;
        };
      }
    ) cfg.instances;

    # Eval-time guards so misconfiguration fails fast with a useful message.
    assertions =
      let
        names = lib.attrNames cfg.instances;
        # Match the alphanumeric + dash whitelist that systemd unit names
        # accept without escaping. Spaces / slashes / unicode in names
        # would silently produce nonsense unit names; reject them up front.
        nameOk = n: builtins.match "[A-Za-z0-9][A-Za-z0-9._-]*" n != null;
        badNames = lib.filter (n: !nameOk n) names;

        dirs = mapAttrsToList (_: i: i.dataDir) cfg.instances;
        users = mapAttrsToList (_: i: i.user) cfg.instances;
      in
      [
        {
          assertion = badNames == [ ];
          message = ''
            services.zeroclaw.instances: instance name(s) ${toString badNames}
            contain characters outside [A-Za-z0-9._-]. Rename them — the
            instance name appears verbatim in the systemd unit name,
            user name, and state directory.
          '';
        }
        {
          assertion = lib.length dirs == lib.length (lib.unique dirs);
          message = ''
            services.zeroclaw.instances: two or more instances declare the
            same dataDir. Each instance needs a unique state directory or
            its SQLite databases will corrupt under concurrent access.
          '';
        }
        {
          assertion = lib.length users == lib.length (lib.unique users);
          message = ''
            services.zeroclaw.instances: two or more instances declare the
            same `user`. If you intend to share a user across instances,
            set `createUser = false` on all but one.
          '';
        }
      ];
  };

  meta = {
    # Filled in by the upstream maintainer when this module lands in the
    # ZeroClaw repository. `[]` rather than a guess so `meta.maintainers`
    # doesn't claim ownership we don't have.
    maintainers = [ ];
  };
}
