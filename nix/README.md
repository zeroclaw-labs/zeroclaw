# NixOS module for ZeroClaw

`nix/module.nix` is a multi-instance NixOS module that runs ZeroClaw under
systemd with sandboxing defaults appropriate for an internet-facing agent
process. It is designed to be importable from any NixOS configuration —
nothing in the module assumes a specific deployment topology.

The shape mirrors `services.restic.backups` (multi-instance Rust services
already in nixpkgs), and the hardening profile mirrors `services.atticd`
(another Rust server in nixpkgs).

This module pairs with the package work in #5987 — the package gives you
`pkgs.zeroclaw`, the module gives you `services.zeroclaw.instances.<name>`.
Either can land first; once both are merged a single-host user can write
`services.zeroclaw.instances.me = { settings = { ... }; };` and have a
running daemon.

## Quick start (single instance)

Add the module to your NixOS configuration's `imports` and declare one
instance:

```nix
{ config, pkgs, ... }: {
  imports = [ ./path/to/zeroclaw/nix/module.nix ];

  # If pkgs.zeroclaw isn't yet in nixpkgs, set the package explicitly:
  # services.zeroclaw.instances.me.package = pkgs.callPackage ./zeroclaw.nix { };

  age.secrets.zeroclaw-bot-token.file = ./secrets/zeroclaw-bot-token.age;

  services.zeroclaw.instances.me = {
    environmentFile = config.age.secrets.zeroclaw-bot-token.path;
    settings = {
      default_provider = "anthropic";
      default_model = "claude-sonnet-4-6";

      channels.telegram = {
        enabled = true;
        # The unit's ExecStartPre runs `envsubst` over the rendered
        # TOML. `$BOT_TOKEN` is read from the EnvironmentFile= and
        # written into ${dataDir}/config.toml (mode 0600, owner =
        # zeroclaw-me). The world-readable copy in /nix/store keeps
        # only the literal "$BOT_TOKEN" placeholder.
        bot_token = "$BOT_TOKEN";
        allowed_users = [ "12345" ];
      };
    };
  };
}
```

After a `nixos-rebuild switch`:

- The unit `zeroclaw-me.service` is started and enabled.
- `/var/lib/zeroclaw-me/` exists, owned by the per-instance user `zeroclaw-me`.
- `/var/lib/zeroclaw-me/config.toml` contains the rendered TOML, mode `0600`.
- ZeroClaw is invoked as `${pkgs.zeroclaw}/bin/zeroclaw daemon`.

## Multi-instance usage

The module is `attrsOf submodule`-shaped, so multiple instances on one host
look identical to one instance:

```nix
services.zeroclaw.instances = {
  alice = { environmentFile = "/run/secrets/alice/identity.env"; settings = { ... }; };
  bob   = { environmentFile = "/run/secrets/bob/identity.env";   settings = { ... }; };
};
```

Each instance gets its own systemd unit, state directory, and per-instance
system user. The module asserts at evaluation time that no two instances
share a `dataDir` or `user`, and that instance names are valid systemd unit
component names (`[A-Za-z0-9._-]+`).

## Option summary

| Option | Type | Default | Purpose |
|---|---|---|---|
| `package` | `package` | `pkgs.zeroclaw` (via `mkPackageOption`) | Override for out-of-tree builds. |
| `user` | `str` | `"zeroclaw-<name>"` | System user. |
| `group` | `str` | `"zeroclaw-<name>"` | System group. |
| `createUser` | `bool` | `true` | Set `false` to bring your own user. |
| `dataDir` | `path` | `"/var/lib/zeroclaw-<name>"` | State directory. Created via `systemd-tmpfiles` so any absolute path works (`/var/lib/...`, `/srv/...`, etc.). |
| `settings` | `submodule { freeformType = (pkgs.formats.toml { }).type; }` | `{}` | Rendered to `${dataDir}/config.toml`. |
| `environmentFile` | `nullOr path` | `null` | systemd `EnvironmentFile=`. Substituted into `settings` strings at start. |
| `extraConfig` | `lines` | `""` | Raw TOML appended after rendered `settings` (escape hatch). |
| `bindReadOnlyPaths` | `attrsOf path` | `{}` | `target → source` map → `BindReadOnlyPaths=`. |

If you need to override a `serviceConfig` field (e.g. add `MemoryMax`),
use the standard NixOS pattern rather than a module-level escape hatch:

```nix
systemd.services."zeroclaw-me".serviceConfig.MemoryMax = lib.mkForce "1G";
```

See `module.nix`'s inline option `description` blocks for the full
contract of each option.

## Secrets pattern

Two paths, both supported, neither leaks secrets to the world-readable
Nix store:

1. **`environmentFile` + `$VAR` substitution in `settings` strings**
   (recommended for channel tokens, webhook secrets, anything ZeroClaw
   doesn't already resolve from the environment natively). Systemd loads
   the file via `EnvironmentFile=` at unit start. The unit's
   `ExecStartPre` then runs `envsubst` over the rendered TOML, expanding
   `$VAR` and `${VAR}` references against the loaded environment, and
   writes the result to `${dataDir}/config.toml` mode `0600` owned by the
   per-instance user. The build-time copy in `/nix/store` only ever
   contains the literal placeholders.

   The substitution is performed by *this module*, not by ZeroClaw —
   ZeroClaw reads `config.toml` verbatim. So this path turns
   `bot_token = "$BOT_TOKEN"` into a working configuration regardless
   of whether ZeroClaw has a native env-var fallback for that field.

2. **`environmentFile` + ZeroClaw-native env-var lookups** for any config
   keys ZeroClaw natively resolves from the environment (e.g.
   `OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `ZEROCLAW_PROVIDER`,
   `ZEROCLAW_MODEL` — see `crates/zeroclaw-config/src/schema.rs`
   upstream for the full list). Same end result — no secret in the
   rendered TOML — and you can omit the field from `settings` entirely.

What the module **never** does: render an interpolated string from a
secret-bearing Nix expression into `settings`. That would put the secret
in the world-readable `/nix/store/.../config.toml`.

When `environmentFile` is set, the unit also gets a
`ConditionPathExists=${environmentFile}` so it stays inactive (rather
than failing) until the file materialises — useful for sops-nix /
agenix activation timing.

## Hardening

Per-instance `serviceConfig` defaults (mirroring `services.atticd`):

```
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
DeviceAllow=
DevicePolicy=closed
ProtectSystem=strict
ProtectHome=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
ProtectProc=invisible
ProcSubset=pid
MemoryDenyWriteExecute=yes
PrivateUsers=yes
RemoveIPC=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
LockPersonality=yes
SystemCallArchitectures=native
CapabilityBoundingSet=
AmbientCapabilities=
SystemCallFilter=@system-service ~@privileged ~@resources
UMask=0077
ReadWritePaths=${dataDir}
```

`MemoryDenyWriteExecute=yes` is safe because ZeroClaw 0.7.x is a plain
Rust binary with no JIT; if a future version adopts a JIT (e.g. through a
WASM plugin host), this single setting will need to flip and that should
be flagged in the changelog.

Resource caps (`MemoryMax`, `CPUQuota`, etc.) are intentionally **not** set
in the module — Rust servers have widely varying resource profiles
depending on workload, and per-host tuning belongs in the caller's config.
To add them, override the generated unit directly:

```nix
systemd.services."zeroclaw-me".serviceConfig = {
  MemoryMax = "1G";
  CPUQuota = "200%";
};
```

## Running the test

The module ships with a NixOS test (`nix/test.nix`) that boots a VM with
multiple instances, validates unit generation, file rendering, multi-instance
isolation, and the hardening profile.

```bash
nix-build -E '
  (import <nixpkgs/nixos/lib/testing-python.nix> { })
    .makeTest (import ./nix/test.nix { })
'
```

Requires KVM on the builder.

## Status

Initial drop, not yet wired into ZeroClaw's CI. The CI workflow at
`.github/workflows/ci.yml` is Rust-only today; adding a `nix-test` job to
exercise `nix/test.nix` is a natural follow-up.
