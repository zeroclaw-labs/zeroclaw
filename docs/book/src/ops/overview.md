# Operations — Overview

How to run ZeroClaw in production. The surface is intentionally small: one binary, one config file, one SQLite workspace. Most "operations" is "systemd and journald".

This section covers:

- [Service & daemon](./service.md) — keeping the process alive
- [Logs & observability](./observability.md) — reading what the agent did
- [Troubleshooting](./troubleshooting.md) — when things break
- [Network deployment](./network-deployment.md) — exposing the gateway, tunnels, reverse proxies

## The shape of a deployment

A typical always-on ZeroClaw install is:

```
zeroclaw service (systemd / launchctl / Windows Service)
  ├── zeroclaw daemon                 — the long-running process
  │   ├── gateway listener (:42617)   — REST / WebSocket / webhooks
  │   ├── channel pollers             — Telegram, IMAP, Nostr relays, etc.
  │   ├── channel listeners           — Discord / Slack / Matrix / WebSocket
  │   ├── cron scheduler              — scheduled SOPs and jobs
  │   └── agent loop (per session)    — provider call + tool execution
  ├── SQLite workspace                — ~/.zeroclaw/workspace/
  ├── config.toml                     — ~/.zeroclaw/config.toml
  ├── tool-receipts log               — ~/.zeroclaw/workspace/receipts/
  └── platform logs                   — journald / launchctl / Event Log
```

Everything except the binary can move — the workspace path is configurable, config paths resolve per environment (Homebrew vs. bootstrap vs. XDG), and log destinations are platform-native by default.

## What to monitor

Four signals matter:

### 1. Service liveness

Is the process running?

```bash
# Linux
systemctl --user is-active zeroclaw

# macOS
launchctl list | grep -c com.zeroclaw.daemon

# Windows
sc query ZeroClaw | findstr STATE
```

If it's dying repeatedly, check [Troubleshooting → Daemon keeps restarting](./troubleshooting.md).

### 2. Channel health

Are channels connected? The gateway exposes `/health/channels`:

```bash
curl -s http://localhost:42617/health/channels | jq
```

```json
{
  "telegram": {"status": "connected", "last_event_ago_secs": 12},
  "discord":  {"status": "connected", "last_event_ago_secs": 4},
  "email":    {"status": "polling",   "next_poll_in_secs": 42},
  "matrix":   {"status": "disconnected", "error": "401 Unauthorized"}
}
```

Monitor `status != "connected"` on push-based channels.

### 3. Provider reliability

Are LLM calls succeeding? `/health/providers`:

```bash
curl -s http://localhost:42617/health/providers | jq
```

```json
{
  "claude": {"ok": true,  "last_latency_ms": 1240, "error_rate_1h": 0.0},
  "local":  {"ok": true,  "last_latency_ms": 3890, "error_rate_1h": 0.0}
}
```

For fallback chains, the meta-provider reports its current working child.

### 4. Tool-call volume and blocks

`/metrics/tools` (Prometheus format):

```
zeroclaw_tool_calls_total{tool="shell",outcome="success"} 342
zeroclaw_tool_calls_total{tool="shell",outcome="blocked"} 4
zeroclaw_tool_calls_total{tool="shell",outcome="denied"} 2
zeroclaw_tool_calls_total{tool="file_write",outcome="success"} 89
```

Blocks and denials are worth looking at — if the agent is repeatedly hitting the same policy block, either your policy is wrong or your agent is misbehaving.

## Capacity

A single ZeroClaw instance can handle:

- Multiple concurrent conversations across all channels
- Tool calls at whatever rate the provider and sandbox allow
- Long-running agent loops (tool chains of 20+ calls)

Scale laterally by running one instance per workspace. Don't try to run two daemons on the same workspace — SQLite's single-writer model will produce lock contention and ultimately corruption.

For multi-tenant hosting, see the proposal in #2765 (closed, historical — the architecture for in-process multi-workspace routing).

## Backups

What to back up:

- `~/.zeroclaw/config.toml` — contains channel credentials (encrypted if using secrets store)
- `~/.zeroclaw/workspace/*.db` — SQLite conversation memory
- `~/.zeroclaw/secrets.key` — master key for the encrypted secrets store (if used). **Without it, the config's secrets are unrecoverable.**
- `~/.zeroclaw/workspace/receipts/` — tool-receipts log

A plain `tar czf zeroclaw-$(date +%F).tar.gz ~/.zeroclaw` covers everything. Restic, borg, or Duplicacy work fine for incremental backups.

**Do not back up `~/.zeroclaw/workspace/cache/`** — it's regenerable and can be large.

## Updates

The service does not auto-update. Subscribe to the release feed (GitHub releases or the Discord `#releases` channel — see [Contributing → Communication](../contributing/communication.md)). Typical update cadence:

1. Read the release notes
2. Back up `~/.zeroclaw/`
3. Update the binary (`brew upgrade`, bootstrap re-run, or `cargo install --force`)
4. `zeroclaw service restart`
5. Verify `/health/*` endpoints return green

If the new version requires config migrations, the startup log emits a warning and the binary usually auto-migrates. Check `zeroclaw config list` to spot-check values after upgrade, and `zeroclaw config migrate` to apply any pending schema migrations manually.

## See also

- [Setup → Service management](../setup/service.md) — install/remove/logs per platform
- [Logs & observability](./observability.md)
- [Troubleshooting](./troubleshooting.md)
- [Network deployment](./network-deployment.md)
