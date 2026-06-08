# Operations: Overview

How to run ZeroClaw in production. The surface is intentionally small: one binary, one config file, one SQLite workspace. Most "operations" is "systemd and journald".

This section covers:

- [Service & daemon](./service.md): keeping the process alive
- [Logs & observability](./observability.md): reading what the agent did
- [Cost tracking](./cost-tracking.md): token spend and per-model cost
- [Troubleshooting](./troubleshooting.md): when things break
- [Network deployment](./network-deployment.md): exposing the gateway, tunnels, reverse proxies

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
  ├── data dir                        — ~/.zeroclaw/data/ (memory/, sessions/, state/)
  ├── config.toml                     — ~/.zeroclaw/config.toml
  ├── .secret_key                     — ~/.zeroclaw/.secret_key (secrets-store master key)
  └── platform logs                   — journald / launchctl / Event Log
```

Everything except the binary can move. The data dir defaults to `~/.zeroclaw/data/` (the legacy `~/.zeroclaw/workspace/` name is still accepted); config paths resolve per environment (Homebrew vs. bootstrap vs. XDG), and log destinations are platform-native by default.

## What to monitor

Four signals matter:

### 1. Service liveness

Is the process running?

<div class="os-tabs-src">

#### Linux

```sh
systemctl --user is-active zeroclaw
```

#### macOS

```sh
launchctl list | grep -c com.zeroclaw.daemon
```

#### Windows

```cmd
schtasks /Query /TN "ZeroClaw Daemon" /FO LIST | findstr Status
```

</div>

If it's dying repeatedly, check [Troubleshooting → Daemon keeps restarting](./troubleshooting.md).

### 2. Channel and component health

The gateway exposes a component health snapshot at `/health` (public, no secrets) and `/api/health` (authenticated). Channels, providers, and other long-running components register themselves in the `components` map as they start, report OK, or error.

<div class="os-tabs-src">

#### sh

```sh
curl -s http://localhost:42617/health | jq
```

</div>

```json
{
  "status": "ok",
  "paired": true,
  "require_pairing": true,
  "runtime": {
    "pid": 4821,
    "updated_at": "2026-06-08T09:00:00+00:00",
    "uptime_seconds": 3600,
    "components": {
      "channel:telegram": {"status": "ok", "updated_at": "…", "last_ok": "…", "last_error": null, "restart_count": 0},
      "channel:matrix":   {"status": "error", "updated_at": "…", "last_ok": "…", "last_error": "401 Unauthorized", "restart_count": 3}
    }
  }
}
```

Each component carries `status` (`starting` / `ok` / `error`), `last_ok`, `last_error`, and `restart_count`. Watch for `status: "error"` and climbing `restart_count`.

### 3. Provider reliability

Providers surface as components in the same `/health` snapshot. For request-level signal (latency, success rate, token counts), scrape `/metrics` (see below) and read `zeroclaw_llm_requests_total` and `zeroclaw_request_latency_seconds`.

### 4. Tool-call volume and metrics

`/metrics` returns Prometheus text exposition. It requires `[observability] backend = "prometheus"` in config; without it the endpoint returns a one-line "backend not enabled" hint.

<div class="os-tabs-src">

#### sh

```sh
curl -s http://localhost:42617/metrics
```

</div>

```
zeroclaw_tool_calls_total{success="true",tool="shell"} 342
zeroclaw_tool_calls_total{success="false",tool="shell"} 6
zeroclaw_tool_calls_total{success="true",tool="file_write"} 89
```

The `zeroclaw_tool_calls_total` counter is labelled by `tool` and `success` (`"true"`/`"false"`). A rising `success="false"` count for one tool is worth looking at: either a policy block, a misbehaving agent, or a flaky tool. Other useful series include `zeroclaw_llm_requests_total`, `zeroclaw_errors_total`, `zeroclaw_active_sessions`, and `zeroclaw_tokens_input_total` / `zeroclaw_tokens_output_total`.

## Capacity

A single ZeroClaw instance can handle:

- Multiple concurrent conversations across all channels
- Tool calls at whatever rate the provider and sandbox allow
- Long-running agent loops (tool chains of 20+ calls)

Scale laterally by running one instance per workspace. Don't try to run two daemons on the same workspace: SQLite's single-writer model will produce lock contention and ultimately corruption.

For multi-tenant hosting, see the proposal in #2765 (closed, historical, the architecture for in-process multi-workspace routing).

## Backups

What to back up:

- `~/.zeroclaw/config.toml`: contains channel credentials (encrypted if using the secrets store)
- `~/.zeroclaw/data/memory/*.db`: SQLite conversation memory (`brain.db`, plus `audit.db`)
- `~/.zeroclaw/data/sessions/`: persisted session state
- `~/.zeroclaw/.secret_key`: master key for the encrypted secrets store (if used). **Without it, the config's encrypted secrets are unrecoverable.**

A plain `tar czf zeroclaw-$(date +%F).tar.gz ~/.zeroclaw` covers everything. Restic, borg, or Duplicacy work fine for incremental backups.

`~/.zeroclaw/data/memory/response_cache.db` is a regenerable LLM response cache; it's safe to include in a full-directory backup or to exclude to save space. Tool receipts are in-band HMAC tokens in the conversation history (see [Tool receipts](../security/tool-receipts.md)), not an on-disk log, so there is nothing separate to back up for them.

## Updates

The service does not auto-update. Subscribe to the release feed (GitHub releases or the Discord `#releases` channel: see [Contributing → Communication](../contributing/communication.md)). Typical update cadence:

1. Read the release notes
2. Back up `~/.zeroclaw/`
3. Update the binary (`brew upgrade`, bootstrap re-run, or `cargo install --force`)
4. `zeroclaw service restart`
5. Verify the `/health` endpoint reports `status: "ok"` with no component in `error`

If the new version requires config migrations, the startup log emits a warning and the binary usually auto-migrates. Check `zeroclaw config list` to spot-check values after upgrade, and `zeroclaw config migrate` to apply any pending schema migrations manually.

## See also

- [Setup → Service management](../setup/service.md): install/remove/logs per platform
- [Logs & observability](./observability.md)
- [Troubleshooting](./troubleshooting.md)
- [Network deployment](./network-deployment.md)
