# ZeroClaw Operations Runbook

This runbook is for operators who maintain availability, security posture, and incident response.

Last verified: **March 12, 2026**.

## Scope

Use this document for day-2 operations:

- starting and supervising runtime
- health checks and diagnostics
- safe rollout and rollback
- incident triage and recovery

For first-time installation, start from [one-click-bootstrap.md](one-click-bootstrap.md).

## Runtime Modes

| Mode | Command | When to use |
|---|---|---|
| Foreground runtime | `zeroclaw daemon` | local debugging, short-lived sessions |
| Foreground gateway only | `zeroclaw gateway` | webhook endpoint testing |
| User service | `zeroclaw service install && zeroclaw service start` | persistent operator-managed runtime |

## Zara Workspace Deployment Boundary

If ZeroClaw is being used as Zara's live brain on this machine, keep the runtime boundary explicit:

- `zara-agent.service` runs `~/.local/bin/zeroclaw` with `WorkingDirectory=~/zara`
- the authoritative ZeroClaw source and build root is `~/zeroclaw`
- `~/zara/zeroclaw` is a workspace mirror/symlink, not the deploy source of truth
- `lifebook-agent.service` is a separate memory/learning service running `~/.local/bin/lifebook-agent` built from `~/lifebook-agent`

For that layout, build from `~/zeroclaw`, deploy the resulting binary to `~/.local/bin/zeroclaw`, and restart `zara-agent.service`.
Do not treat edits under `~/zara/zeroclaw` as live until the real binary has been rebuilt and deployed.

## Baseline Operator Checklist

1. Validate configuration:

```bash
zeroclaw status
```

2. Verify diagnostics:

```bash
zeroclaw doctor
zeroclaw channel doctor
```

3. Start runtime:

```bash
zeroclaw daemon
```

4. For persistent user session service:

```bash
zeroclaw service install
zeroclaw service start
zeroclaw service status
```

## Health and State Signals

| Signal | Command / File | Expected |
|---|---|---|
| Config validity | `zeroclaw doctor` | no critical errors |
| Channel connectivity | `zeroclaw channel doctor` | configured channels healthy |
| Runtime summary | `zeroclaw status` | expected provider/model/channels |
| Daemon heartbeat/state | `~/.zeroclaw/daemon_state.json` | file updates periodically |

## Logs and Diagnostics

### macOS / Windows (service wrapper logs)

- `~/.zeroclaw/logs/daemon.stdout.log`
- `~/.zeroclaw/logs/daemon.stderr.log`

### Linux (systemd user service)

```bash
journalctl --user -u zeroclaw.service -f
```

## Incident Triage Flow (Fast Path)

1. Snapshot system state:

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

2. Check service state:

```bash
zeroclaw service status
```

3. If service is unhealthy, restart cleanly:

```bash
zeroclaw service stop
zeroclaw service start
```

4. If channels still fail, verify allowlists and credentials in `~/.zeroclaw/config.toml`.

5. If gateway is involved, verify bind/auth settings (`[gateway]`) and local reachability.

## Safe Change Procedure

Before applying config changes:

1. backup `~/.zeroclaw/config.toml`
2. apply one logical change at a time
3. run `zeroclaw doctor`
4. restart daemon/service
5. verify with `status` + `channel doctor`

If you are operating the Zara deployment layout:

1. edit ZeroClaw code in `~/zeroclaw`
2. rebuild from `~/zeroclaw`
3. stop `zara-agent.service`
4. copy `~/zeroclaw/target/release/zeroclaw` to `~/.local/bin/zeroclaw`
5. restart `zara-agent.service`
6. if Lifebook changed, rebuild from `~/lifebook-agent`, deploy `~/.local/bin/lifebook-agent`, then restart `lifebook-agent.service`

## Rollback Procedure

If a rollout regresses behavior:

1. restore previous `config.toml`
2. restart runtime (`daemon` or `service`)
3. confirm recovery via `doctor` and channel health checks
4. document incident root cause and mitigation

## Related Docs

- [one-click-bootstrap.md](one-click-bootstrap.md)
- [troubleshooting.md](troubleshooting.md)
- [config-reference.md](config-reference.md)
- [commands-reference.md](commands-reference.md)
