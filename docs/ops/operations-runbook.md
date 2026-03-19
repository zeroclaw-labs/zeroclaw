# JhedaiClaw Operations Runbook

This runbook is for operators who maintain availability, security posture, and incident response.

Last verified: **February 18, 2026**.

## Scope

Use this document for day-2 operations:

- starting and supervising runtime
- health checks and diagnostics
- safe rollout and rollback
- incident triage and recovery

For first-time installation, start from [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md).

## Runtime Modes

| Mode                    | Command                                                  | When to use                           |
| ----------------------- | -------------------------------------------------------- | ------------------------------------- |
| Foreground runtime      | `jhedaiclaw daemon`                                      | local debugging, short-lived sessions |
| Foreground gateway only | `jhedaiclaw gateway`                                     | webhook endpoint testing              |
| User service            | `jhedaiclaw service install && jhedaiclaw service start` | persistent operator-managed runtime   |

## Baseline Operator Checklist

1. Validate configuration:

```bash
jhedaiclaw status
```

2. Verify diagnostics:

```bash
jhedaiclaw doctor
jhedaiclaw channel doctor
```

3. Start runtime:

```bash
jhedaiclaw daemon
```

4. For persistent user session service:

```bash
jhedaiclaw service install
jhedaiclaw service start
jhedaiclaw service status
```

## Health and State Signals

| Signal                 | Command / File                    | Expected                         |
| ---------------------- | --------------------------------- | -------------------------------- |
| Config validity        | `jhedaiclaw doctor`               | no critical errors               |
| Channel connectivity   | `jhedaiclaw channel doctor`       | configured channels healthy      |
| Runtime summary        | `jhedaiclaw status`               | expected provider/model/channels |
| Daemon heartbeat/state | `~/.jhedaiclaw/daemon_state.json` | file updates periodically        |

## Logs and Diagnostics

### macOS / Windows (service wrapper logs)

- `~/.jhedaiclaw/logs/daemon.stdout.log`
- `~/.jhedaiclaw/logs/daemon.stderr.log`

### Linux (systemd user service)

```bash
journalctl --user -u jhedaiclaw.service -f
```

## Incident Triage Flow (Fast Path)

1. Snapshot system state:

```bash
jhedaiclaw status
jhedaiclaw doctor
jhedaiclaw channel doctor
```

2. Check service state:

```bash
jhedaiclaw service status
```

3. If service is unhealthy, restart cleanly:

```bash
jhedaiclaw service stop
jhedaiclaw service start
```

4. If channels still fail, verify allowlists and credentials in `~/.jhedaiclaw/config.toml`.

5. If gateway is involved, verify bind/auth settings (`[gateway]`) and local reachability.

## Safe Change Procedure

Before applying config changes:

1. backup `~/.jhedaiclaw/config.toml`
2. apply one logical change at a time
3. run `jhedaiclaw doctor`
4. restart daemon/service
5. verify with `status` + `channel doctor`

## Rollback Procedure

If a rollout regresses behavior:

1. restore previous `config.toml`
2. restart runtime (`daemon` or `service`)
3. confirm recovery via `doctor` and channel health checks
4. document incident root cause and mitigation

## Related Docs

- [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md)
- [troubleshooting.md](./troubleshooting.md)
- [config-reference.md](../reference/api/config-reference.md)
- [commands-reference.md](../reference/cli/commands-reference.md)
